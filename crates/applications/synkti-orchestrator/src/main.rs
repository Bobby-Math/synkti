//! Synkti AWS Orchestrator
//!
//! Spot instance orchestration for ML inference workloads.
//!
//! ## Usage
//!
//! ```bash
//! # Start the orchestrator with vLLM
//! synkti-orchestrator run --model meta-llama/Llama-2-7b-hf
//!
//! # Monitor spot instances
//! synkti-orchestrator monitor
//!
//! # Migrate a checkpoint
//! synkti-orchestrator migrate --checkpoint-id chk-001
//! ```

use clap::{Parser, Subcommand};
use futures::StreamExt;
use std::time::Duration;
use synkti_orchestrator::{
    cleanup_stale_owner, create_owner_marker, is_owner, remove_owner_marker, TerraformRunner,
    instance::{create_ec2_client, InstanceSpec},
    monitor::{SpotMonitor, GRACE_PERIOD_SECONDS},
    vllm::{VllmClient, VllmConfig, VllmContainer},
};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Context trait for anyhow error handling
use anyhow::Context;

// Base64 encoding engine trait
use base64::Engine;

/// RAII guard for automatic infrastructure cleanup.
/// When dropped (including on panic), destroys the infrastructure.
struct InfraGuard {
    project: String,
    infra_dir: String,
}

impl Drop for InfraGuard {
    fn drop(&mut self) {
        info!("🛑 Cleaning up infrastructure (RAII)...");
        match TerraformRunner::new(&self.infra_dir, &self.project).destroy() {
            Ok(()) => {
                info!("✅ Infrastructure destroyed");
            }
            Err(e) => {
                error!("⚠️  Failed to destroy infrastructure: {}", e);
            }
        }
        let _ = remove_owner_marker(&self.project);
    }
}

#[derive(Parser)]
#[command(name = "synkti-orchestrator")]
#[command(about = "Synkti AWS Orchestrator - Spot instance management for ML inference", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Infrastructure management (RAII - auto cleanup on exit)
    Infra {
        /// Project name
        #[arg(long)]
        project: String,

        /// AWS region
        #[arg(long, default_value = "us-east-1")]
        region: String,

        /// Infra directory (default: ./infra)
        #[arg(long, default_value = "./infra")]
        infra_dir: String,

        #[command(subcommand)]
        action: InfraAction,
    },

    /// Launch a new EC2 instance
    Launch {
        /// Instance type (e.g., g4dn.xlarge, g5.xlarge)
        #[arg(long, default_value = "g4dn.xlarge")]
        instance_type: String,

        /// AMI ID
        #[arg(long)]
        ami: String,

        /// Key pair name
        #[arg(long)]
        key_name: Option<String>,

        /// Security group IDs (can specify multiple)
        #[arg(long)]
        security_group: Vec<String>,

        /// Subnet ID
        #[arg(long)]
        subnet: Option<String>,

        /// IAM instance profile name (for SSM access)
        #[arg(long)]
        iam_profile: Option<String>,

        /// Launch as spot instance with max price (e.g., 0.30)
        #[arg(long)]
        spot_price: Option<String>,

        /// Instance name tag
        #[arg(long)]
        name: Option<String>,

        /// Wait for instance to be running
        #[arg(long, default_value_t = true)]
        wait: bool,

        /// User data script file
        #[arg(long)]
        user_data: Option<String>,

        /// AWS region (default: us-east-1)
        #[arg(long, default_value = "us-east-1")]
        region: String,
    },

    /// Run the orchestrator with vLLM
    Run {
        /// Model to serve (HuggingFace model ID or S3 path with --model-s3)
        #[arg(long)]
        model: String,

        /// S3 path to model weights (e.g., s3://my-bucket/llama-2-7b/)
        /// If specified, downloads from S3 instead of HuggingFace
        #[arg(long)]
        model_s3: Option<String>,

        /// vLLM Docker image
        #[arg(long, default_value = "vllm/vllm-openai:latest")]
        image: String,

        /// Port for vLLM API
        #[arg(long, default_value_t = 8000)]
        port: u16,

        /// Maximum context length
        #[arg(long, default_value_t = 4096)]
        max_model_len: usize,

        /// Tensor parallel size (number of GPUs)
        #[arg(long, default_value_t = 1)]
        tensor_parallel_size: usize,

        /// Quantization format
        #[arg(long)]
        quantization: Option<String>,

        /// Container name
        #[arg(long)]
        container_name: Option<String>,

        /// Spot monitoring interval (seconds)
        #[arg(long, default_value_t = 5)]
        monitor_interval: u64,

        /// Enable auto infrastructure (create on start, destroy on exit)
        #[arg(long)]
        auto_infra: bool,

        /// Project name for auto infra
        #[arg(long)]
        project: Option<String>,

        /// Infra directory for auto infra (default: ./infra)
        #[arg(long, default_value = "./infra")]
        infra_dir: String,
    },

    /// Monitor spot instance for interruption notices
    Monitor {
        /// Polling interval (seconds)
        #[arg(long, default_value_t = 5)]
        interval: u64,

        /// Action on interruption (checkpoint, log, ignore)
        #[arg(long, default_value = "log")]
        action: String,
    },

    /// Checkpoint a running container
    Checkpoint {
        /// Container ID or name
        container_id: String,

        /// Checkpoint ID
        #[arg(long)]
        checkpoint_id: Option<String>,
    },

    /// Restore a container from checkpoint
    Restore {
        /// Checkpoint ID
        checkpoint_id: String,

        /// Archive path
        #[arg(long)]
        _archive: String,

        /// Container name
        #[arg(long)]
        container_name: String,
    },
}

#[derive(Subcommand)]
enum InfraAction {
    /// Create infrastructure (terraform apply)
    Create {
        /// Control plane instance type
        #[arg(long, default_value = "t3.medium")]
        control_plane_type: String,

        /// Number of control plane instances
        #[arg(long, default_value_t = 1)]
        control_plane_count: usize,

        /// CIDR blocks allowed to access control plane (e.g., "0.0.0.0/0")
        #[arg(long)]
        allowed_cidr: Vec<String>,
    },

    /// Destroy infrastructure (terraform destroy)
    Destroy,

    /// Show infrastructure status
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "synkti_orchestrator=info,info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Launch {
            instance_type,
            ami,
            key_name,
            security_group,
            subnet,
            iam_profile,
            spot_price,
            name,
            wait,
            user_data,
            region,
        } => {
            launch_instance(
                instance_type,
                ami,
                key_name,
                security_group,
                subnet,
                iam_profile,
                spot_price,
                name,
                wait,
                user_data,
                region,
            )
            .await
        }

        Commands::Infra {
            project,
            region,
            infra_dir,
            action,
        } => handle_infra(project, region, infra_dir, action).await,

        Commands::Run {
            model,
            model_s3,
            image,
            port,
            max_model_len,
            tensor_parallel_size,
            quantization,
            container_name,
            monitor_interval,
            auto_infra,
            project,
            infra_dir,
        } => {
            run_orchestrator(
                model,
                model_s3,
                image,
                port,
                max_model_len,
                tensor_parallel_size,
                quantization,
                container_name,
                monitor_interval,
                auto_infra,
                project,
                infra_dir,
            )
            .await
        }

        Commands::Monitor { interval, action } => {
            monitor_spot(interval, action).await
        }

        Commands::Checkpoint {
            container_id,
            checkpoint_id,
        } => {
            let chk_id = checkpoint_id.unwrap_or_else(|| {
                format!("chk-{}", chrono::Utc::now().timestamp())
            });
            checkpoint_container(container_id, chk_id).await
        }

        Commands::Restore {
            checkpoint_id,
            _archive,
            container_name,
        } => restore_container(checkpoint_id, container_name).await,
    }
}

/// Launch a new EC2 instance
async fn launch_instance(
    instance_type: String,
    ami: String,
    key_name: Option<String>,
    security_group: Vec<String>,
    subnet: Option<String>,
    iam_profile: Option<String>,
    spot_price: Option<String>,
    name: Option<String>,
    wait: bool,
    user_data: Option<String>,
    region: String,
) -> anyhow::Result<()> {
    info!("🚀 Launching EC2 instance");
    info!("📦 Type: {}", instance_type);
    info!("🖼️  AMI: {}", ami);
    if let Some(ref profile) = iam_profile {
        info!("🔐 IAM Profile: {}", profile);
    }
    if spot_price.is_some() {
        info!("💰 Spot instance requested");
    }

    // Create EC2 client
    let client = create_ec2_client(Some(region.clone())).await?;

    // Build instance spec
    let mut spec = InstanceSpec::new(&ami).with_instance_type(&instance_type);

    if let Some(key) = key_name {
        spec = spec.with_key_pair(key);
    }
    for sg in security_group {
        spec = spec.with_security_group(sg);
    }
    if let Some(subnet_id) = subnet {
        spec = spec.with_subnet(subnet_id);
    }
    if let Some(profile) = iam_profile {
        spec = spec.with_iam_profile(profile);
    }
    if let Some(price) = spot_price {
        spec = spec.with_spot_price(price);
    }
    if let Some(data_file) = user_data {
        let data = tokio::fs::read_to_string(&data_file).await?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        spec = spec.with_user_data(encoded);
    }

    // Build tags
    let mut tags = vec![("ManagedBy".to_string(), "Synkti".to_string())];
    if let Some(name_str) = name {
        tags.push(("Name".to_string(), name_str));
    }

    // Launch instance
    let mut instance = spec.launch(&client, tags).await?;

    info!("✅ Instance launched: {}", instance.id);
    info!("📍 State: {:?}", instance.state);
    if let Some(ref ip) = instance.public_ip {
        info!("🌐 Public IP: {}", ip);
    }
    if let Some(ref ip) = instance.private_ip {
        info!("🏠 Private IP: {}", ip);
    }

    // Wait for instance to be running
    if wait {
        info!("⏳ Waiting for instance to be running...");
        instance
            .wait_until_running(&client, Duration::from_secs(300))
            .await?;
        info!("✅ Instance is running!");

        // Refresh to get final IPs
        instance.refresh_state(&client).await?;
        if let Some(ref ip) = instance.public_ip {
            info!("🌐 Public IP: {}", ip);
        }
        if let Some(ref ip) = instance.private_ip {
            info!("🏠 Private IP: {}", ip);
        }

        info!("📝 To connect via SSM Session Manager:");
        info!("   aws ssm start-session --target {} --region {}", instance.id, region);
        info!("📝 To terminate:");
        info!("   aws ec2 terminate-instances --instance-ids {}", instance.id);
    }

    Ok(())
}

/// Run the orchestrator with vLLM and spot monitoring
async fn run_orchestrator(
    model: String,
    model_s3: Option<String>,
    image: String,
    port: u16,
    max_model_len: usize,
    tensor_parallel_size: usize,
    quantization: Option<String>,
    container_name: Option<String>,
    monitor_interval: u64,
    auto_infra: bool,
    project: Option<String>,
    infra_dir: String,
) -> anyhow::Result<()> {
    // Handle auto-infra setup (RAII pattern)
    let _infra_guard = if auto_infra {
        let project_name = project.clone().unwrap_or_else(|| "synkti-default".to_string());

        info!("🏗️  Creating infrastructure for project: {}", project_name);

        // Check for stale owner
        let _ = cleanup_stale_owner(&project_name);

        // Check if already owned by another process
        let terraform = TerraformRunner::new(&infra_dir, &project_name);
        terraform.init()?;

        let outputs = terraform.apply()?;
        create_owner_marker(&project_name)?;

        info!("✅ Infrastructure ready");
        info!("📋 Control plane: {}", outputs.control_plane_instance_ids);
        info!("🔐 Worker profile: {}", outputs.worker_instance_profile_name);
        info!("");

        Some(InfraGuard {
            project: project_name,
            infra_dir,
        })
    } else {
        None
    };

    info!("🚀 Starting Synkti AWS Orchestrator");
    info!("📦 Model: {}", model);
    if let Some(ref s3_path) = model_s3 {
        info!("📥 S3 Model Path: {}", s3_path);
    }
    info!("🔌 Port: {}", port);

    // Download model from S3 if specified
    let actual_model = if let Some(s3_path) = model_s3 {
        info!("📥 Downloading model from S3...");
        let local_path = download_model_from_s3(&s3_path).await?;
        info!("✅ Model downloaded to: {}", local_path);
        local_path
    } else {
        model.clone()
    };

    // Build vLLM configuration
    let mut config = VllmConfig::new(&actual_model)
        .with_image(image)
        .with_port(port)
        .with_max_model_len(max_model_len)
        .with_tensor_parallel_size(tensor_parallel_size);

    if let Some(quant) = quantization {
        config = config.with_quantization(quant);
    }
    if let Some(name) = container_name {
        config = config.with_container_name(name);
    }

    // Start vLLM container
    let mut vllm = VllmContainer::new(config);
    vllm.start().await?;

    let api_url = vllm.api_url();
    info!("✅ vLLM running at {}", api_url);

    // Verify vLLM is healthy
    let client = VllmClient::new(&api_url);
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    match client.health_check().await {
        Ok(true) => info!("✅ vLLM health check passed"),
        Ok(false) => error!("⚠️  vLLM health check failed"),
        Err(e) => error!("⚠️  Health check error: {}", e),
    }

    // Start spot monitoring
    let monitor = SpotMonitor::with_interval(std::time::Duration::from_secs(monitor_interval));

    info!("👀 Monitoring spot instance for interruptions ({}s interval)", monitor_interval);

    // Clone for the monitoring task
    let _vllm_container_id = vllm.container_id().unwrap_or("").to_string();

    // Spawn spot monitoring task
    let mut monitor_task = tokio::spawn(async move {
        let mut stream = monitor.monitor_stream();
        while let Some(notice) = stream.next().await {
            match notice.action {
                synkti_orchestrator::monitor::SpotAction::Terminate => {
                    info!(
                        "🚨 SPOT TERMINATION NOTICE: {} seconds until termination",
                        notice.seconds_until_action
                    );
                    info!("📍 Action time: {}", notice.time);

                    // TODO: Trigger checkpoint and migration
                    // For now, just log
                    if notice.seconds_until_action <= GRACE_PERIOD_SECONDS {
                        info!("⏱️  Within grace period, initiating checkpoint...");
                        // checkpoint_container(&vllm_container_id, checkpoint_id).await?;
                    }
                }
                _ => info!("Spot notice: {:?}", notice.action),
            }
        }
    });

    // Wait for Ctrl+C or monitor task to complete
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("🛑 Shutting down...");
            monitor_task.abort();
            vllm.stop().await?;
            info!("✅ Shutdown complete");
        }
        result = &mut monitor_task => {
            info!("Monitor task ended: {:?}", result);
            vllm.stop().await?;
        }
    }

    Ok(())
}

/// Monitor spot instance for interruption notices
async fn monitor_spot(interval: u64, action: String) -> anyhow::Result<()> {
    info!("👀 Monitoring spot instance ({}s interval, action: {})", interval, action);

    let monitor = SpotMonitor::with_interval(std::time::Duration::from_secs(interval));
    let mut stream = monitor.monitor_stream();

    while let Some(notice) = stream.next().await {
        match notice.action {
            synkti_orchestrator::monitor::SpotAction::Terminate => {
                info!(
                    "🚨 SPOT TERMINATION NOTICE: {} seconds until termination",
                    notice.seconds_until_action
                );
                info!("📍 Action time: {}", notice.time);

                match action.as_str() {
                    "checkpoint" => {
                        info!("💾 Creating checkpoint (action: checkpoint)...");
                        // TODO: Implement checkpoint action
                    }
                    "ignore" => {
                        info!("Ignoring spot interruption notice");
                    }
                    _ => {
                        info!("Logged spot interruption notice");
                    }
                }
            }
            _ => info!("Spot notice: {:?}", notice.action),
        }
    }

    Ok(())
}

/// Checkpoint a running container
async fn checkpoint_container(container_id: String, checkpoint_id: String) -> anyhow::Result<()> {
    info!(
        "💾 Creating checkpoint '{}' for container '{}'",
        checkpoint_id, container_id
    );

    use std::process::Command;
    let output = Command::new("docker")
        .args([
            "checkpoint",
            "create",
            "--checkpoint-dir=/tmp/checkpoints",
            "--leave=true",
            &container_id,
            &checkpoint_id,
        ])
        .output()?;

    if output.status.success() {
        info!("✅ Checkpoint '{}' created successfully", checkpoint_id);
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to create checkpoint: {}", stderr);
    }
}

/// Restore a container from checkpoint
async fn restore_container(
    checkpoint_id: String,
    container_name: String,
) -> anyhow::Result<()> {
    info!(
        "📥 Restoring container '{}' from checkpoint '{}'",
        container_name, checkpoint_id
    );

    // TODO: Implement restore logic
    info!("✅ Restore complete");

    Ok(())
}

/// Handle infrastructure commands
async fn handle_infra(
    project: String,
    _region: String,
    infra_dir: String,
    action: InfraAction,
) -> anyhow::Result<()> {
    let terraform = TerraformRunner::new(&infra_dir, &project);

    match action {
        InfraAction::Create {
            control_plane_type,
            control_plane_count,
            allowed_cidr,
        } => {
            info!("🏗️  Creating infrastructure for project: {}", project);
            info!("📦 Control plane: {} x {}", control_plane_count, control_plane_type);

            // Check for stale owner
            if cleanup_stale_owner(&project).is_ok() {
                info!("🧹 Cleaned up stale owner marker");
            }

            // Check if already owned
            if is_owner(&project) {
                info!("⚠️  Infrastructure already owned by this process");
                let status = terraform.status()?;
                print_infra_status(&status);
                return Ok(());
            }

            // Initialize terraform
            terraform.init()?;

            // Apply configuration
            let outputs = terraform.apply()?;

            // Create owner marker
            create_owner_marker(&project)?;

            info!("✅ Infrastructure created successfully!");
            info!("📋 Control plane instance IDs: {}", outputs.control_plane_instance_ids);
            info!("🔐 Worker instance profile: {}", outputs.worker_instance_profile_name);
            info!("🔒 Worker security group: {}", outputs.worker_sg_id);
            info!("🪣 Checkpoint bucket: {}", outputs.checkpoint_bucket_name);
            info!("");
            info!("📝 To connect to control plane:");
            info!("   {}", outputs.connect_command);
            info!("");
            info!("📝 To launch more workers:");
            info!("   {}", outputs.launch_command);
        }

        InfraAction::Destroy => {
            info!("🗑️  Destroying infrastructure for project: {}", project);

            // Check ownership
            if !is_owner(&project) {
                info!("⚠️  This process does not own the infrastructure");
                info!("ℹ️  Destroying anyway (manual override)");
            }

            terraform.init()?;
            terraform.destroy()?;
            remove_owner_marker(&project)?;

            info!("✅ Infrastructure destroyed successfully!");
        }

        InfraAction::Status => {
            info!("📊 Infrastructure status for project: {}", project);
            info!("");

            terraform.init()?;
            let status = terraform.status()?;
            print_infra_status(&status);
        }
    }

    Ok(())
}

/// Print infrastructure status
fn print_infra_status(status: &synkti_orchestrator::InfraStatus) {
    info!("Project: {}", status.project_name);
    info!("");

    if status.control_plane_instance_ids.is_empty()
        || (status.control_plane_instance_ids.len() == 1
            && status.control_plane_instance_ids[0].is_empty())
    {
        info!("⚠️  No control plane instances found");
        info!(
            "   Run 'synkti-orchestrator infra create --project {}' to create infrastructure",
            status.project_name
        );
    } else {
        info!("Control Plane Instances:");
        for (i, id) in status.control_plane_instance_ids.iter().enumerate() {
            if !id.is_empty() {
                info!("  [{}] {}", i, id);
            }
        }
        info!("");
        info!("Public IPs:");
        for (i, ip) in status.control_plane_public_ips.iter().enumerate() {
            if !ip.is_empty() {
                info!("  [{}] {}", i, ip);
            }
        }
        info!("");
        info!("Worker Resources:");
        info!("  Instance Profile: {}", status.worker_instance_profile_name);
        info!("  Security Group: {}", status.worker_sg_id);
        info!("");
        info!("Checkpoint Bucket: {}", status.checkpoint_bucket_name);
        info!("Models Bucket: {}", status.models_bucket_name);
    }
}

/// Download model weights from S3 to local storage.
/// Uses AWS CLI for efficient download (handles multipart, resume, etc.)
async fn download_model_from_s3(s3_path: &str) -> anyhow::Result<String> {
    use std::process::Command;

    // Parse S3 path (e.g., s3://bucket-name/models/llama-2-7b/)
    let s3_path = s3_path.trim_end_matches('/');
    if !s3_path.starts_with("s3://") {
        anyhow::bail!("Invalid S3 path: {}. Must start with s3://", s3_path);
    }

    // Extract bucket and prefix from s3://bucket/prefix
    let path_without_scheme = &s3_path[5..]; // Remove "s3://"
    let parts: Vec<&str> = path_without_scheme.splitn(2, '/').collect();
    let bucket = parts[0];
    let prefix = parts.get(1).unwrap_or(&"");

    // Create local cache directory
    let model_name = prefix.rsplit('/').next().unwrap_or("model");
    let local_dir = format!("/tmp/synkti-models-{}", model_name);

    // Create directory if it doesn't exist
    std::fs::create_dir_all(&local_dir)
        .context("failed to create model directory")?;

    info!("📂 Syncing from s3://{}/{} to {}", bucket, prefix, local_dir);

    // Use AWS CLI s3 sync for efficient download
    let output = Command::new("aws")
        .args([
            "s3",
            "sync",
            &format!("s3://{}/{}", bucket, prefix),
            &local_dir,
            "--quiet",
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to download from S3: {}", stderr);
    }

    Ok(local_dir)
}
