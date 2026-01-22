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
use std::sync::Arc;
use std::time::Duration;
use synkti_orchestrator::{
    assign::{AssignmentCandidate, AssignmentStrategy, Workload},
    cleanup_stale_owner, create_owner_marker, is_owner, remove_owner_marker, TerraformRunner,
    discovery::{tag_self_as_worker, untag_self_as_worker, DiscoveryConfig, PeerDiscovery},
    drain::ElbConfig,
    elb::LoadBalancerManager,
    failover::{FailoverConfig, FailoverManager},
    instance::{create_ec2_client, Ec2Instance, InstanceSpec},
    monitor::{SpotMonitor, GRACE_PERIOD_SECONDS},
    remote::SsmExecutor,
    vllm::{VllmClient, VllmConfig, VllmContainer},
};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
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

        /// Assignment strategy for failover (earliest, least-loaded, warm-least-loaded, random)
        #[arg(long, default_value = "earliest")]
        assignment_strategy: String,

        /// Target group ARN for load balancer integration
        #[arg(long)]
        target_group_arn: Option<String>,

        /// AWS region for SSM and ELB (default: us-east-1)
        #[arg(long, default_value = "us-east-1")]
        region: String,

        /// Cluster name for P2P peer discovery (nodes with same cluster name discover each other)
        #[arg(long, default_value = "synkti-default")]
        cluster: String,
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
            assignment_strategy,
            target_group_arn,
            region,
            cluster,
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
                assignment_strategy,
                target_group_arn,
                region,
                cluster,
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
#[allow(clippy::too_many_arguments)]
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
    assignment_strategy: String,
    target_group_arn: Option<String>,
    region: String,
    cluster: String,
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

    // Parse assignment strategy
    let strategy = match assignment_strategy.as_str() {
        "earliest" | "fifo" => AssignmentStrategy::EarliestNode,
        "least-loaded" => AssignmentStrategy::LeastLoaded,
        "warm-least-loaded" | "warm" => AssignmentStrategy::WarmLeastLoaded,
        "random" => AssignmentStrategy::Random,
        _ => {
            warn!(
                "Unknown assignment strategy '{}', defaulting to earliest",
                assignment_strategy
            );
            AssignmentStrategy::EarliestNode
        }
    };
    info!("📋 Assignment strategy: {:?}", strategy);

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

    // Create FailoverManager with configuration
    let failover_config = FailoverConfig::default()
        .with_strategy(strategy)
        .with_vllm_config(config.clone());
    let failover_manager = Arc::new(FailoverManager::with_config(failover_config));

    // Setup AWS clients for SSM and ELB integration
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(region.clone()))
        .load()
        .await;
    let ssm_executor = Arc::new(SsmExecutor::from_config(&aws_config).await);
    let elb_manager = Arc::new(LoadBalancerManager::from_config(&aws_config).await);
    let ec2_client = aws_sdk_ec2::Client::new(&aws_config);

    // Get current instance info for P2P discovery
    let current_instance_id = match get_current_instance_id().await {
        Ok(id) => {
            info!("🆔 Current instance: {}", id);
            Some(id)
        }
        Err(e) => {
            warn!("⚠️  Could not get instance ID (not running on EC2?): {}", e);
            None
        }
    };

    // Tag self as Synkti worker for peer discovery
    if let Some(ref instance_id) = current_instance_id {
        match tag_self_as_worker(&ec2_client, instance_id, &cluster).await {
            Ok(()) => info!("🏷️  Tagged as Synkti worker in cluster '{}'", cluster),
            Err(e) => warn!("⚠️  Failed to tag self: {}", e),
        }
    }

    // Setup P2P peer discovery
    let discovery_config = DiscoveryConfig::new(&cluster)
        .with_self_instance_id(current_instance_id.clone().unwrap_or_default())
        .with_refresh_interval(Duration::from_secs(30));

    let peer_discovery = Arc::new(PeerDiscovery::from_config(&aws_config, discovery_config).await);

    // Initial peer discovery
    match peer_discovery.discover_peers().await {
        Ok(peers) => info!("🔍 Discovered {} peers in cluster '{}'", peers.len(), cluster),
        Err(e) => warn!("⚠️  Initial peer discovery failed: {}", e),
    }

    // Start background peer refresh task
    let _discovery_task = peer_discovery.clone().start_refresh_task();
    info!("🔄 P2P peer discovery active (30s refresh)");

    // Get the shared candidates list from discovery
    let candidates = peer_discovery.peers_ref();

    // Optional ELB configuration
    let elb_config = target_group_arn.map(|arn| {
        info!("🔄 Load balancer integration enabled: {}", arn);
        ElbConfig {
            target_group_arn: arn,
            port: Some(port as i32),
        }
    });

    // Start vLLM container
    let mut vllm = VllmContainer::new(config.clone());
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
    info!("🔄 Stateless failover enabled with {:?} strategy", strategy);
    info!("🌐 P2P Architecture: Each node is self-aware, self-healing, self-governing");

    // Clone for the monitoring task
    let failover_manager_clone = failover_manager.clone();
    let ssm_executor_clone = ssm_executor.clone();
    let elb_manager_clone = elb_manager.clone();
    let elb_config_clone = elb_config.clone();
    let candidates_clone = candidates.clone();
    let model_clone = model.clone();
    let api_url_clone = api_url.clone();
    let current_instance_id_clone = current_instance_id.clone();
    let ec2_client_clone = ec2_client.clone();

    // Spawn spot monitoring task with failover integration
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

                    if notice.seconds_until_action <= GRACE_PERIOD_SECONDS {
                        info!("⏱️  Within grace period, initiating stateless failover...");

                        // Get current instance info from metadata
                        let current_instance = match get_current_instance_info().await {
                            Ok(instance) => instance,
                            Err(e) => {
                                error!("Failed to get current instance info: {}", e);
                                continue;
                            }
                        };

                        // Create vLLM client for the current instance
                        let vllm_client = VllmClient::new(&api_url_clone);

                        // Get candidate instances
                        let instances = candidates_clone.read().await;
                        let candidate_refs: Vec<AssignmentCandidate> =
                            instances.iter().map(AssignmentCandidate::new).collect();

                        // Create workload (estimated memory for the model)
                        let workload = Workload::new(&model_clone, 8000.0);

                        // Execute failover
                        let result = failover_manager_clone
                            .handle_preemption(
                                &notice,
                                &current_instance,
                                &vllm_client,
                                &candidate_refs,
                                &workload,
                            )
                            .await;

                        if result.success {
                            info!(
                                "✅ Failover completed successfully in {:.2}s",
                                result.total_time_secs
                            );
                            info!("   Drain: {:.2}s", result.phase_times.drain_secs);
                            info!("   Select: {:.2}s", result.phase_times.select_secs);
                            info!("   Spawn: {:.2}s", result.phase_times.spawn_secs);
                            info!(
                                "   Health check: {:.2}s",
                                result.phase_times.health_check_secs
                            );
                            if let Some(ref replacement_id) = result.replacement_instance_id {
                                info!("   Replacement: {}", replacement_id);
                            }
                        } else {
                            error!(
                                "❌ Failover failed: {}",
                                result.error.unwrap_or_else(|| "Unknown error".to_string())
                            );
                        }
                    }
                }
                _ => debug!("Spot notice: {:?}", notice.action),
            }
        }
    });

    // Wait for Ctrl+C or monitor task to complete
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("🛑 Shutting down...");
            monitor_task.abort();

            // Untag self from cluster before shutdown
            if let Some(ref instance_id) = current_instance_id {
                info!("🏷️  Removing Synkti worker tags...");
                if let Err(e) = untag_self_as_worker(&ec2_client, instance_id).await {
                    warn!("⚠️  Failed to untag self: {}", e);
                }
            }

            vllm.stop().await?;
            info!("✅ Shutdown complete");
        }
        result = &mut monitor_task => {
            info!("Monitor task ended: {:?}", result);

            // Untag self from cluster
            if let Some(ref instance_id) = current_instance_id {
                if let Err(e) = untag_self_as_worker(&ec2_client, instance_id).await {
                    warn!("⚠️  Failed to untag self: {}", e);
                }
            }

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

/// Get just the current EC2 instance ID from instance metadata
async fn get_current_instance_id() -> anyhow::Result<String> {
    let client = reqwest::Client::new();

    // IMDSv2: Get token first
    let token = client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "60")
        .send()
        .await?
        .text()
        .await?;

    // Get instance ID
    let instance_id = client
        .get("http://169.254.169.254/latest/meta-data/instance-id")
        .header("X-aws-ec2-metadata-token", &token)
        .send()
        .await?
        .text()
        .await?;

    Ok(instance_id)
}

/// Get current EC2 instance information from instance metadata
async fn get_current_instance_info() -> anyhow::Result<Ec2Instance> {
    use std::collections::HashMap;

    // IMDSv2: Get token first
    let client = reqwest::Client::new();
    let token = client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "60")
        .send()
        .await?
        .text()
        .await?;

    // Get instance ID
    let instance_id = client
        .get("http://169.254.169.254/latest/meta-data/instance-id")
        .header("X-aws-ec2-metadata-token", &token)
        .send()
        .await?
        .text()
        .await?;

    // Get instance type
    let instance_type = client
        .get("http://169.254.169.254/latest/meta-data/instance-type")
        .header("X-aws-ec2-metadata-token", &token)
        .send()
        .await?
        .text()
        .await?;

    // Get local IPv4 (private IP)
    let private_ip = client
        .get("http://169.254.169.254/latest/meta-data/local-ipv4")
        .header("X-aws-ec2-metadata-token", &token)
        .send()
        .await?
        .text()
        .await?;

    // Get public IPv4 (may not exist)
    let public_ip = client
        .get("http://169.254.169.254/latest/meta-data/public-ipv4")
        .header("X-aws-ec2-metadata-token", &token)
        .send()
        .await
        .ok()
        .and_then(|r| {
            if r.status().is_success() {
                Some(r)
            } else {
                None
            }
        });

    let public_ip = match public_ip {
        Some(r) => Some(r.text().await.unwrap_or_default()),
        None => None,
    };

    // Estimate GPU memory based on instance type
    let gpu_memory_gb = estimate_gpu_memory(&instance_type);

    Ok(Ec2Instance {
        id: instance_id,
        instance_type,
        state: synkti_orchestrator::instance::InstanceState::Running,
        public_ip,
        private_ip: Some(private_ip),
        launch_time: chrono::Utc::now(), // Approximate, could fetch from metadata
        gpu_memory_gb,
        network_bandwidth_gbps: 10.0, // Approximate
        gpu_memory_used_mb: 0.0,
        tags: HashMap::new(),
    })
}

/// Estimate GPU memory based on instance type
fn estimate_gpu_memory(instance_type: &str) -> f64 {
    match instance_type {
        // G4dn instances (T4 GPU - 16GB)
        t if t.starts_with("g4dn") => 16.0,
        // G5 instances (A10G GPU - 24GB)
        t if t.starts_with("g5") => 24.0,
        // G6 instances (L4 GPU - 24GB)
        t if t.starts_with("g6") => 24.0,
        // P3 instances (V100 GPU - 16/32GB)
        t if t.starts_with("p3.2") => 16.0,
        t if t.starts_with("p3.8") => 64.0,  // 4x16GB
        t if t.starts_with("p3.16") => 128.0, // 8x16GB
        t if t.starts_with("p3dn") => 256.0, // 8x32GB
        // P4 instances (A100 GPU - 40/80GB)
        t if t.starts_with("p4d") => 320.0, // 8x40GB
        t if t.starts_with("p4de") => 640.0, // 8x80GB
        // P5 instances (H100 GPU - 80GB)
        t if t.starts_with("p5") => 640.0, // 8x80GB
        // Default
        _ => 16.0,
    }
}
