//! Synkti - P2P Spot Instance Orchestration
//!
//! ## RAII Philosophy
//!
//! Synkti is a responsible intelligence that borrows cloud resources temporarily
//! and returns them promptly. When synkti exits (gracefully or via panic), it
//! cleans up all resources it created. This embodies RAII at the system level.
//!
//! ## Usage
//!
//! ```bash
//! # Run orchestrator (auto-creates infra if needed)
//! synkti --project-name synkti-prod
//!
//! # Infrastructure management
//! synkti infra create --project-name synkti-prod
//! synkti infra destroy --project-name synkti-prod
//!
//! # Worker management (RAII-style: synkti supervises the worker)
//! synkti worker launch --project-name synkti-prod
//! # ^ Press Ctrl+C to exit and auto-terminate worker
//!
//! # Spot monitoring only (no orchestrator)
//! synkti monitor
//! ```

use clap::{Parser, Subcommand};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use synkti_orchestrator::{
    assign::{AssignmentCandidate, AssignmentStrategy, Workload},
    cleanup_stale_owner, create_owner_marker, is_owner, remove_owner_marker, TerraformRunner,
    discovery::{tag_self_as_worker, untag_self_as_worker, DiscoveryConfig, PeerDiscovery},
    elb::LoadBalancerManager,
    failover::FailoverManager,
    instance::Ec2Instance,
    monitor::{SpotMonitor, GRACE_PERIOD_SECONDS},
    remote::SsmExecutor,
    vllm::{VllmClient, VllmConfig, VllmContainer},
};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use aws_sdk_ec2::Client as Ec2Client;

/// RAII guard for self-termination.
///
/// Runs on the EC2 instance itself. When synkti exits (gracefully or via panic),
/// this guard terminates the instance it's running on. This implements the principle
/// that synkti is a responsible intelligence that borrows resources and returns them.
struct SelfTerminatingGuard {
    instance_id: String,
    region: String,
}

impl SelfTerminatingGuard {
    /// Create a new self-terminating guard.
    fn new(instance_id: String, region: String) -> Self {
        Self { instance_id, region }
    }

    /// Terminate this instance.
    fn terminate(&self) {
        info!("🛑 Terminating this instance {}", self.instance_id);
        match std::process::Command::new("aws")
            .args([
                "ec2",
                "terminate-instances",
                "--instance-ids",
                &self.instance_id,
                "--region",
                &self.region,
            ])
            .output()
        {
            Ok(output) if output.status.success() => {
                info!("✅ Self-termination initiated");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("⚠️  Failed to terminate: {}", stderr);
            }
            Err(e) => {
                warn!("⚠️  Failed to run aws command: {}", e);
            }
        }
    }
}

impl Drop for SelfTerminatingGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            error!("💥 PANIC! Self-terminating to return borrowed resources");
        } else {
            info!("👋 Synkti exiting. Self-terminating to return borrowed resources");
        }
        self.terminate();
    }
}

/// Synkti: P2P Spot Instance Orchestration for ML Inference
#[derive(Parser)]
#[command(name = "synkti")]
#[command(about = "P2P spot instance orchestration for ML inference", long_about = None)]
struct Cli {
    /// Project name (namespaces all resources)
    #[arg(long, global = true)]
    project_name: Option<String>,

    /// AWS region (default: us-east-1)
    #[arg(long, global = true, default_value = "us-east-1")]
    region: String,

    /// Infra directory for Terraform configs (default: ./infra)
    #[arg(long, global = true, default_value = "./infra")]
    infra_dir: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Infrastructure management (terraform apply/destroy)
    Infra {
        #[command(subcommand)]
        action: InfraAction,
    },

    /// Worker instance management (launch, list, terminate)
    Worker {
        #[command(subcommand)]
        action: WorkerAction,
    },

    /// Monitor spot instance for interruption notices (standalone, no orchestrator)
    Monitor {
        /// Polling interval (seconds)
        #[arg(long, default_value_t = 5)]
        interval: u64,

        /// Action on interruption (log, checkpoint)
        #[arg(long, default_value = "log")]
        action: String,
    },

    /// Checkpoint a running container (for testing)
    Checkpoint {
        /// Container ID or name
        container_id: String,

        /// Checkpoint ID
        #[arg(long)]
        checkpoint_id: Option<String>,
    },

    /// Restore a container from checkpoint (for testing)
    Restore {
        /// Checkpoint ID
        checkpoint_id: String,

        /// Container name
        #[arg(long)]
        container_name: String,
    },
}

#[derive(Subcommand)]
enum InfraAction {
    /// Create infrastructure (terraform apply)
    Create {
        /// Worker instance type
        #[arg(long, default_value = "g4dn.xlarge")]
        worker_type: String,

        /// Number of worker instances to launch
        #[arg(long, default_value_t = 0)]
        worker_count: usize,

        /// CIDR blocks allowed to access workers
        #[arg(long, default_value = "0.0.0.0/0")]
        allowed_cidr: Vec<String>,
    },

    /// Destroy infrastructure (terraform destroy)
    Destroy {
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },

    /// Show infrastructure outputs (bucket names, instance IDs, etc.)
    Status,
}

#[derive(Subcommand)]
enum WorkerAction {
    /// Launch a new spot worker instance
    Launch {
        /// Instance type (e.g., g4dn.xlarge, g5.xlarge)
        #[arg(long, default_value = "g4dn.xlarge")]
        instance_type: String,

        /// AMI ID (optional, auto-detected based on instance type)
        #[arg(long)]
        ami: Option<String>,

        /// IAM instance profile name
        #[arg(long)]
        iam_profile: Option<String>,

        /// Security group IDs
        #[arg(long)]
        security_groups: Vec<String>,

        /// Subnet ID
        #[arg(long)]
        subnet: Option<String>,

        /// Key pair name
        #[arg(long)]
        key_pair: Option<String>,

        /// User data script file
        #[arg(long)]
        user_data: Option<String>,

        /// Spot maximum price (USD/hour, empty = on-demand price)
        #[arg(long)]
        spot_price: Option<String>,

        /// Wait for instance to be running
        #[arg(long)]
        wait: bool,
    },

    /// List all worker instances
    List {
        /// Show detailed information
        #[arg(long)]
        detailed: bool,
    },

    /// Terminate a worker instance
    Terminate {
        /// Instance ID to terminate
        instance_id: String,

        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "synkti=info,info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    // Handle subcommands
    if let Some(command) = cli.command {
        return match command {
            Commands::Infra { action } => {
                let project = cli.project_name.ok_or_else(|| {
                    anyhow::anyhow!("--project-name required for infra commands")
                })?;
                handle_infra(project, cli.region, cli.infra_dir, action).await
            }

            Commands::Worker { action } => {
                let project = cli.project_name.ok_or_else(|| {
                    anyhow::anyhow!("--project-name required for worker commands")
                })?;
                handle_worker(project, cli.region, cli.infra_dir, action).await
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
                container_name,
            } => restore_container(checkpoint_id, container_name).await,
        };
    }

    // Default: context-aware behavior
    let project = cli.project_name.ok_or_else(|| {
        anyhow::anyhow!("--project-name required")
    })?;

    // Detect context: are we running on EC2 or locally?
    let on_ec2 = is_running_on_ec2().await;

    if on_ec2 {
        info!("🖥️  Running on EC2 - Orchestrator Mode");
        run_orchestrator(
            project,
            cli.region,
            cli.infra_dir,
        )
        .await
    } else {
        info!("💻 Running locally - Deployment Mode");
        deploy_instances(
            project,
            cli.region,
            cli.infra_dir,
        )
        .await
    }
}

/// Handle infrastructure commands
async fn handle_infra(
    project: String,
    region: String,
    infra_dir: String,
    action: InfraAction,
) -> anyhow::Result<()> {
    let terraform = TerraformRunner::new(&infra_dir, &project);

    match action {
        InfraAction::Create {
            worker_type,
            worker_count,
            allowed_cidr,
        } => {
            info!("🏗️  Creating infrastructure for project: {}", project);

            // Check for stale owner
            let _ = cleanup_stale_owner(&project);

            terraform.init()?;

            let cidr_arg = if allowed_cidr.is_empty() {
                "[\"0.0.0.0/0\"]".to_string()
            } else {
                format!("[{}]", allowed_cidr.iter().map(|s| format!("\"{}\"", s)).collect::<Vec<_>>().join(","))
            };

            let output = std::process::Command::new("terraform")
                .args([
                    "apply",
                    "-auto-approve",
                    &format!("-var=project_name={}", project),
                    &format!("-var=aws_region={}", region),
                    &format!("-var=worker_instance_type={}", worker_type),
                    &format!("-var=worker_count={}", worker_count),
                    &format!("-var=allowed_cidr_blocks={}", cidr_arg),
                ])
                .current_dir(&infra_dir)
                .output()?;

            if output.status.success() {
                create_owner_marker(&project)?;
                let outputs = terraform.parse_outputs()?;
                print_infra_outputs(&outputs);
                info!("✅ Infrastructure created successfully");
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("terraform apply failed: {}", stderr);
            }
        }

        InfraAction::Destroy { force } => {
            if !force {
                println!("⚠️  This will destroy all infrastructure for '{}'", project);
                print!("Continue? [y/N]: ");
                use std::io::Write;
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().to_lowercase().starts_with('y') {
                    info!("Aborted");
                    return Ok(());
                }
            }

            info!("🗑️  Destroying infrastructure for project: {}", project);
            terraform.init()?;
            terraform.destroy()?;
            remove_owner_marker(&project)?;
            info!("✅ Infrastructure destroyed");
            Ok(())
        }

        InfraAction::Status => {
            info!("📋 Infrastructure status for: {}", project);
            let outputs = terraform.parse_outputs()?;
            print_infra_outputs(&outputs);
            Ok(())
        }
    }
}

/// Handle worker commands
async fn handle_worker(
    project: String,
    region: String,
    infra_dir: String,
    action: WorkerAction,
) -> anyhow::Result<()> {
    use synkti_orchestrator::instance::{
        create_ec2_client, get_gpu_ami, get_standard_ami, is_gpu_instance_type,
        list_workers, terminate_worker, InstanceSpec,
    };

    let ec2_client = create_ec2_client(Some(region.clone())).await?;

    match action {
        WorkerAction::Launch {
            instance_type,
            ami,
            iam_profile,
            security_groups,
            subnet,
            key_pair,
            user_data,
            spot_price: _,
            wait,
        } => {
            info!("🚀 Launching worker instance for project: {}", project);
            info!("   Instance type: {}", instance_type);

            // Get AMI ID
            let ami_id = if let Some(ami) = ami {
                ami
            } else {
                // Auto-detect AMI based on instance type
                if is_gpu_instance_type(&instance_type) {
                    info!("🔍 Detecting GPU AMI...");
                    get_gpu_ami(&ec2_client, &region).await?
                } else {
                    info!("🔍 Detecting standard AMI...");
                    get_standard_ami(&ec2_client, &region).await?
                }
            };
            info!("   AMI: {}", ami_id);

            // Get IAM profile from terraform outputs if not specified
            let iam_profile = if let Some(profile) = iam_profile {
                profile
            } else {
                let terraform = TerraformRunner::new(&infra_dir, &project);
                match terraform.get_output("worker_instance_profile_name") {
                    Ok(profile) => {
                        info!("   IAM profile: {} (from terraform)", profile);
                        profile
                    }
                    Err(_) => {
                        warn!("⚠️  No IAM profile found, instance may not have SSM access");
                        String::new()
                    }
                }
            };

            // Get security groups from terraform if not specified
            let security_groups = if security_groups.is_empty() {
                let terraform = TerraformRunner::new(&infra_dir, &project);
                match terraform.get_output("worker_sg_id") {
                    Ok(sg_id) => {
                        info!("   Security group: {} (from terraform)", sg_id);
                        vec![sg_id]
                    }
                    Err(_) => {
                        warn!("⚠️  No security groups specified");
                        vec![]
                    }
                }
            } else {
                security_groups
            };

            // Get models bucket for user data template
            let terraform = TerraformRunner::new(&infra_dir, &project);
            let models_bucket = match terraform.get_output("models_bucket_name") {
                Ok(bucket) => bucket,
                Err(_) => {
                    warn!("⚠️  Could not get models bucket for user data");
                    format!("{}-models", project)
                }
            };

            // Read user data from file if specified, or use default from infra directory
            let user_data_explicitly_provided = user_data.is_some();
            let user_data_file = if let Some(file_path) = user_data {
                file_path
            } else {
                // Default to user-data.sh in infra directory
                format!("{}/user-data.sh", infra_dir)
            };

            let user_data_content = match std::fs::read_to_string(&user_data_file) {
                Ok(mut content) => {
                    // Template variables (same as terraform templatefile)
                    content = content.replace("${project_name}", &project);
                    content = content.replace("${models_bucket}", &models_bucket);
                    content = content.replace("${region}", &region);
                    content = content.replace("${synkti_binary_s3_path}", &format!("s3://{}/bin/synkti", models_bucket));
                    content = content.replace("${model_s3_path}", &format!("s3://{}/qwen2.5-7b/", models_bucket));
                    content = content.replace("${huggingface_model}", "Qwen/Qwen2.5-7B-Instruct");

                    info!("   User data: {}", user_data_file);
                    use base64::prelude::*;
                    Some(BASE64_STANDARD.encode(content))
                }
                Err(e) => {
                    if user_data_explicitly_provided {
                        anyhow::bail!("Failed to read user data file: {}", e);
                    } else {
                        // User data file is optional if not explicitly specified
                        warn!("⚠️  No user data file found at {} - instance will not have vLLM", user_data_file);
                        None
                    }
                }
            };

            // Build instance spec
            let mut spec = InstanceSpec::new(&ami_id)
                .with_instance_type(&instance_type)
                .with_iam_profile(&iam_profile)
                .with_spot_price(""); // Empty = on-demand price cap

            for sg in &security_groups {
                spec = spec.with_security_group(sg);
            }

            if let Some(subnet) = subnet {
                spec = spec.with_subnet(subnet);
            }

            if let Some(key_pair) = key_pair {
                spec = spec.with_key_pair(key_pair);
            }

            if let Some(user_data) = user_data_content {
                spec = spec.with_user_data(user_data);
            }

            // Launch instance with project tags
            let tags = vec![
                ("Name".to_string(), format!("{}-worker", project)),
                ("SynktiCluster".to_string(), project.clone()),
                ("SynktiRole".to_string(), "worker".to_string()),
                ("ManagedBy".to_string(), "Synkti".to_string()),
                ("Project".to_string(), project.clone()),
            ];

            let mut instance = spec.launch(&ec2_client, tags).await?;
            info!("✅ Instance launched: {}", instance.id);

            // Wait for running if requested
            if wait {
                info!("⏳ Waiting for instance to be running...");
                instance.wait_until_running(&ec2_client, std::time::Duration::from_secs(300)).await?;
                info!("✅ Instance is running");
                if let Some(ip) = &instance.public_ip {
                    info!("   Public IP: {}", ip);
                }
                if let Some(ip) = &instance.private_ip {
                    info!("   Private IP: {}", ip);
                }
            }

            // Fire and forget: instance runs independently with its own RAII
            info!("");
            info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            info!("🚀 Worker {} is now running independently", instance.id);
            info!("   RAII is active ON THE INSTANCE: if synkti crashes there,");
            info!("   the instance will self-terminate to return borrowed resources.");
            info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

            Ok(())
        }

        WorkerAction::List { detailed } => {
            info!("📋 Listing workers for project: {}", project);

            let workers = list_workers(&ec2_client, &project).await?;

            if workers.is_empty() {
                info!("⚠️  No workers found");
                return Ok(());
            }

            info!("Found {} worker(s)", workers.len());
            info!("");
            info!("{:<20} {:<15} {:<12} {:<18}", "Instance ID", "State", "Type", "IP Address");
            info!("{:-<20} {:-<15} {:-<12} {:-<18}", "───────", "─────", "─────", "─────");

            for worker in &workers {
                let state_str = format!("{:?}", worker.state);
                info!(
                    "{:<20} {:<15} {:<12} {:<18}",
                    worker.id,
                    state_str,
                    worker.instance_type,
                    worker.private_ip.as_ref().unwrap_or(&"N/A".to_string())
                );

                if detailed {
                    info!("   Launch time: {}", worker.launch_time);
                    info!("   GPU memory: {} GB", worker.gpu_memory_gb);
                    if let Some(public_ip) = &worker.public_ip {
                        info!("   Public IP: {}", public_ip);
                    }
                    info!("");
                }
            }

            Ok(())
        }

        WorkerAction::Terminate {
            instance_id,
            force,
        } => {
            if !force {
                println!("⚠️  This will terminate instance '{}'", instance_id);
                print!("Continue? [y/N]: ");
                use std::io::Write;
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().to_lowercase().starts_with('y') {
                    info!("Aborted");
                    return Ok(());
                }
            }

            info!("🗑️  Terminating worker: {}", instance_id);
            terminate_worker(&ec2_client, &instance_id).await?;
            info!("✅ Worker termination initiated");
            Ok(())
        }
    }
}

fn print_infra_outputs(outputs: &synkti_orchestrator::TerraformOutputs) {
    info!("Models bucket: {}", outputs.models_bucket_name);
    info!("Checkpoint bucket: {}", outputs.checkpoint_bucket_name);
    info!("Worker profile: {}", outputs.worker_instance_profile_name);
}

/// Detect if running on EC2 using multiple heuristics
///
/// Uses a layered approach to detect EC2 environment:
/// 1. IMDSv2 token check (primary)
/// 2. Instance identity document verification (secondary)
/// 3. System UUID check (tertiary, Linux-specific)
///
/// Returns true if ANY check indicates we're on EC2.
async fn is_running_on_ec2() -> bool {
    // Check 1: IMDSv2 token availability
    if check_imdsv2_token().await {
        debug!("✓ EC2 detected via IMDSv2 token");
        return true;
    }

    // Check 2: Try to get instance identity document (more reliable)
    if check_instance_identity().await {
        debug!("✓ EC2 detected via instance identity document");
        return true;
    }

    // Check 3: System UUID check (Linux DMI - EC2 uses "ec2" prefix)
    if check_system_uuid() {
        debug!("✓ EC2 detected via system UUID");
        return true;
    }

    debug!("✗ Not running on EC2 (local machine)");
    false
}

/// Check 1: IMDSv2 token endpoint
async fn check_imdsv2_token() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    match client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "60")
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

/// Check 2: Instance identity document
async fn check_instance_identity() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    // First get token
    let token = match client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "60")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
        _ => return false,
    };

    if token.is_empty() {
        return false;
    }

    // Try to get identity document
    match client
        .get("http://169.254.169.254/latest/dynamic/instance-identity/document")
        .header("X-aws-ec2-metadata-token", token)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            // Verify it's valid JSON with expected fields
            if let Ok(text) = response.text().await {
                text.contains("\"region\"") && text.contains("\"instanceId\"")
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Check 3: System UUID (DMI on Linux)
///
/// EC2 instances have UUIDs starting with "ec2"
/// File: /sys/hypervisor/uuid (Xen) or /sys/class/dmi/id/product_uuid
fn check_system_uuid() -> bool {
    // Check Xen hypervisor UUID (older EC2 instances)
    if let Ok(content) = std::fs::read_to_string("/sys/hypervisor/uuid") {
        if content.trim().starts_with("ec2") {
            return true;
        }
    }

    // Check DMI product UUID (newer EC2 instances)
    if let Ok(content) = std::fs::read_to_string("/sys/class/dmi/id/product_uuid") {
        let content = content.trim().to_lowercase();
        // EC2 UUIDs contain "ec2" or start with specific patterns
        if content.contains("ec2") || content.starts_with("33") {
            return true;
        }
    }

    false
}

/// Deployment Mode: Monitoring dashboard for the cluster
///
/// This is called when synkti is run locally (not on EC2).
/// It verifies infra and dependencies, then monitors the cluster.
///
/// Ctrl+C exits the local monitor ONLY - AWS instances continue running.
async fn deploy_instances(
    project: String,
    region: String,
    infra_dir: String,
) -> anyhow::Result<()> {
    info!("🚀 Deployment Mode for project: {}", project);
    info!("🌍 Region: {}", region);
    info!("💡 Press Ctrl+C to exit monitor (instances continue running)");
    info!("");

    // 1. Ensure infrastructure exists (auto-create if missing)
    let terraform = TerraformRunner::new(&infra_dir, &project);
    if !is_owner(&project) {
        warn!("⚠️  Infrastructure not found for project '{}'", project);
        info!("🏗️  Creating infrastructure automatically...");
        info!("   This will create: S3 buckets, IAM roles, security groups");

        match terraform.apply() {
            Ok(_) => {
                info!("✅ Infrastructure created successfully");
                create_owner_marker(&project)?;
            }
            Err(e) => {
                error!("❌ Failed to create infrastructure: {}", e);
                error!("   Run manually: terraform -chdir={} apply -var=project_name={}", infra_dir, project);
                return Err(e);
            }
        }
    } else {
        info!("✅ Infrastructure exists");
    }

    // 2. Get infrastructure outputs
    let outputs = terraform.parse_outputs()?;
    let bucket_name = &outputs.models_bucket_name;
    info!("📋 Models bucket: {}", bucket_name);

    // 3. Check dependencies (binary + model in S3)
    let aws_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3_client = aws_sdk_s3::Client::new(&aws_config);
    let ec2_client = Ec2Client::new(&aws_config);

    // The bucket name already has the project prefix
    let bucket_for_check = bucket_name;

    info!("🔍 Checking dependencies in s3://{}", bucket_for_check);

    // Check binary exists
    let binary_key = "bin/synkti";

    match s3_client
        .head_object()
        .bucket(bucket_for_check)
        .key(binary_key)
        .send()
        .await
    {
        Ok(_) => info!("✅ Orchestrator binary found"),
        Err(_) => {
            error!("❌ Orchestrator binary NOT found in S3");
            error!("   Expected at: s3://{}/{}", bucket_for_check, binary_key);
            error!("");
            error!("💡 Upload the binary:");
            error!("   ./scripts/upload-binary.sh --project-name {}", project);
            error!("");
            error!("💡 Or run bootstrap:");
            error!("   ./scripts/bootstrap.sh --project-name {}", project);
            return Err(anyhow::anyhow!("Missing orchestrator binary"));
        }
    }

    // Check model exists
    let models = s3_client
        .list_objects_v2()
        .bucket(bucket_for_check)
        .delimiter("/")
        .send()
        .await?;

    let has_models = models
        .common_prefixes()
        .iter()
        .filter(|p| {
            p.prefix()
                .and_then(|p| p.strip_suffix('/'))
                .map(|p| p != "bin")
                .unwrap_or(false)
        })
        .count() > 0;

    if !has_models {
        error!("❌ No model weights found in S3");
        error!("   Expected at: s3://{}/<model-name>/", bucket_for_check);
        error!("");
        error!("💡 Upload model weights:");
        error!("   ./scripts/upload-model.sh --project-name {} --model <MODEL_ID>", project);
        error!("   Example: ./scripts/upload-model.sh --project-name {} --model Qwen/Qwen2.5-7B-Instruct", project);
        return Err(anyhow::anyhow!("Missing model weights"));
    }
    info!("✅ Model weights found");

    info!("");
    info!("✅ All dependencies verified!");

    // 4. Check if instances are running
    let tag_key = "SynktiCluster";
    let tag_value = &project;

    let response = ec2_client
        .describe_instances()
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name(format!("tag:{}", tag_key))
                .values(tag_value)
                .build(),
        )
        .send()
        .await?;

    let instances: Vec<_> = response
        .reservations()
        .iter()
        .flat_map(|r| r.instances().iter())
        .collect();

    if instances.is_empty() {
        warn!("⚠️  No instances found for cluster '{}'", project);
        info!("");
        info!("💡 Launch spot instances with:");
        info!("   cd {}", infra_dir);
        info!("   terraform apply -var=project_name={} -var=worker_count=<N>", project);
        info!("");
        info!("   Or run: synkti infra create --project-name {} --worker-count 1", project);
        info!("");
        info!("Each instance will automatically:");
        info!("   1. Download orchestrator binary from S3");
        info!("   2. Download model weights from S3");
        info!("   3. Run: synkti --project-name {} (Orchestrator Mode)", project);
        return Err(anyhow::anyhow!("No instances running"));
    }

    info!("🔍 Found {} instance(s) in cluster", instances.len());

    // 5. Enter monitoring loop
    info!("");
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    info!("📊 Cluster Monitor: {}", project);
    info!("   Refreshing every 10 seconds. Ctrl+C to exit (instances continue)");
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let mut interval = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Query instances
                let response = match ec2_client
                    .describe_instances()
                    .filters(
                        aws_sdk_ec2::types::Filter::builder()
                            .name(format!("tag:{}", tag_key))
                            .values(tag_value)
                            .build(),
                    )
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        error!("Failed to query instances: {}", e);
                        continue;
                    }
                };

                let instances: Vec<_> = response
                    .reservations()
                    .iter()
                    .flat_map(|r| r.instances().iter())
                    .collect();

                // Clear screen and show status
                print!("\x1b[2J\x1b[H"); // Clear screen, move cursor to top
                info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                info!("📊 Cluster: {} | 📍 Region: {} | 🔢 Nodes: {}", project, region, instances.len());
                info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                info!("");

                if instances.is_empty() {
                    warn!("⚠️  No instances found. Launch with:");
                    warn!("   terraform -chdir={} apply -var=project_name={} -var=worker_count=1", infra_dir, project);
                } else {
                    info!("{:<20} {:<15} {:<15} {:<12} {:<18}", "Instance ID", "State", "Type", "Spot?", "IP Address");
                    info!("{:-<20} {:-<15} {:-<15} {:-<12} {:-<18}", "───────", "─────", "─────", "─────", "─────");

                    for inst in &instances {
                        let id = inst.instance_id().unwrap_or("unknown");
                        let state_name = inst
                            .state()
                            .and_then(|s| s.name())
                            .map(|n| n.as_str())
                            .unwrap_or("unknown");
                        let itype = inst.instance_type().map(|t| t.as_str()).unwrap_or("unknown");
                        let instance_lifecycle = inst.instance_lifecycle();
                        let private_ip = inst.private_ip_address().unwrap_or("N/A");

                        let state_icon = match state_name {
                            "running" => "🟢",
                            "pending" => "🟡",
                            "shutting-down" | "terminated" => "🔴",
                            "stopping" | "stopped" => "⚫",
                            _ => "⚪",
                        };

                        let is_spot = matches!(instance_lifecycle, Some(aws_sdk_ec2::types::InstanceLifecycleType::Spot));

                        info!("{:<20} {:<14} {:<15} {:<12} {:<18}",
                            format!("{} {}", state_icon, id),
                            state_name,
                            itype,
                            if is_spot { "Yes" } else { "No" },
                            private_ip
                        );
                    }
                }

                info!("");
                info!("Press Ctrl+C to exit monitor (AWS instances continue running)");
            }

            _ = tokio::signal::ctrl_c() => {
                info!("");
                info!("👋 Exiting monitor. AWS instances continue running.");
                info!("   To terminate instances: terraform -chdir={} destroy -var=project_name={}", infra_dir, project);
                return Ok(());
            }
        }
    }
}

/// Run the orchestrator with P2P peer discovery and spot failover
#[allow(clippy::too_many_arguments)]
async fn run_orchestrator(
    project: String,
    region: String,
    infra_dir: String,
) -> anyhow::Result<()> {
    info!("🚀 Synkti Orchestrator starting");
    info!("📦 Project: {}", project);
    info!("🌍 Region: {}", region);

    // Get current instance ID early for RAII guard
    let current_instance_id = match get_current_instance_id().await {
        Ok(id) => {
            info!("🆔 Current instance: {}", id);
            id
        }
        Err(e) => {
            anyhow::bail!("Not running on EC2, cannot use RAII: {}", e);
        }
    };

    // RAII: If this synkti process exits or crashes, terminate this instance
    // This embodies the principle: synkti is a responsible intelligence that
    // borrows resources and returns them promptly.
    let _self_guard = SelfTerminatingGuard::new(current_instance_id.clone(), region.clone());
    info!("🛡️  RAII active: This instance will auto-terminate if synkti exits");

    // Ensure infrastructure exists
    let terraform = TerraformRunner::new(&infra_dir, &project);
    if !is_owner(&project) {
        info!("🏗️  Infrastructure not found, creating...");
        terraform.init()?;
        let _ = cleanup_stale_owner(&project);
        terraform.apply()?;
        create_owner_marker(&project)?;
        info!("✅ Infrastructure ready");
    }

    // Get infrastructure outputs
    let outputs = terraform.parse_outputs()?;
    info!("📋 Models bucket: {}", outputs.models_bucket_name);
    info!("🔐 Worker profile: {}", outputs.worker_instance_profile_name);

    // Build AWS config
    let aws_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;

    // EC2 client for tagging and discovery
    let ec2_client = aws_sdk_ec2::Client::new(&aws_config);

    // Cluster name = project name for P2P discovery
    let cluster_name = project.clone();

    // Tag self as Synkti worker for peer discovery
    match tag_self_as_worker(&ec2_client, &current_instance_id, &cluster_name).await {
        Ok(()) => info!("🏷️  Tagged as worker in cluster '{}'", cluster_name),
        Err(e) => warn!("⚠️  Failed to tag self: {}", e),
    }

    // Setup P2P peer discovery
    let discovery_config = DiscoveryConfig::new(&cluster_name)
        .with_self_instance_id(current_instance_id.clone())
        .with_refresh_interval(Duration::from_secs(30));

    let peer_discovery = Arc::new(PeerDiscovery::from_config(&aws_config, discovery_config).await);

    // Initial peer discovery
    match peer_discovery.discover_peers().await {
        Ok(peers) => info!("🔍 Discovered {} peers", peers.len()),
        Err(e) => warn!("⚠️  Initial peer discovery failed: {}", e),
    }

    // Start background peer refresh task
    let _discovery_task = peer_discovery.clone().start_refresh_task();
    info!("🔄 P2P peer discovery active (30s refresh)");

    // Get the shared candidates list from discovery
    let candidates = peer_discovery.peers_ref();

    // Model configuration
    let model = "Qwen/Qwen2.5-7B-Instruct".to_string();
    let model_s3 = Some(format!("s3://{}/qwen2.5-7b/", outputs.models_bucket_name));

    // vLLM configuration
    let vllm_config = VllmConfig {
        image: "vllm/vllm-openai:latest".to_string(),
        model: model.clone(),
        port: 8000,
        max_model_len: 4096,
        tensor_parallel_size: 1,
        quantization: None,
        gpu_memory_utilization: 0.9,
        host: "0.0.0.0".to_string(),
        container_name: Some("synkti-vllm".to_string()),
    };

    // Start vLLM container
    info!("🤖 Starting vLLM container...");
    let mut vllm = VllmContainer::new(vllm_config.clone());
    let api_url = vllm.start().await?;
    info!("✅ vLLM started at: {}", api_url);

    // Wait for vLLM to be ready (health check with timeout)
    info!("⏳ Waiting for vLLM to be ready...");
    let vllm_client = VllmClient::new(&api_url);
    let start_time = std::time::Instant::now();
    let timeout = Duration::from_secs(300);

    while start_time.elapsed() < timeout {
        if vllm_client.health_check().await.unwrap_or(false) {
            info!("✅ vLLM is ready");
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Setup failover manager
    let ssm_executor = Arc::new(SsmExecutor::from_config(&aws_config).await);
    let elb_manager = Arc::new(LoadBalancerManager::from_config(&aws_config).await);

    // Failover config with WarmLeastLoaded strategy (recommended)
    let failover_config = synkti_orchestrator::failover::FailoverConfig {
        assignment_strategy: AssignmentStrategy::WarmLeastLoaded,
        drain_timeout: Duration::from_secs(115),
        health_check_timeout: Duration::from_secs(120),
        vllm_config: vllm_config.clone(),
    };

    let failover_manager = Arc::new(FailoverManager::with_config(failover_config));

    // Spot monitoring
    let monitor_interval = 5; // seconds

    info!("👀 Monitoring spot instance ({}s interval)", monitor_interval);
    info!("🔄 Stateless failover enabled");
    info!("🌐 P2P Architecture: Each node is self-aware, self-healing, self-governing");

    // Clone for the monitoring task
    let failover_manager_clone = failover_manager.clone();
    let candidates_clone = candidates.clone();
    let model_clone = model.clone();
    let api_url_clone = api_url.clone();

    // Spawn spot monitoring task with failover integration
    let mut monitor_task = tokio::spawn(async move {
        let monitor = SpotMonitor::with_interval(Duration::from_secs(monitor_interval));
        let mut stream = monitor.monitor_stream();

        while let Some(notice) = stream.next().await {
            match notice.action {
                synkti_orchestrator::monitor::SpotAction::Terminate => {
                    info!(
                        "🚨 SPOT TERMINATION NOTICE: {} seconds until termination",
                        notice.seconds_until_action
                    );

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
            info!("🏷️  Removing worker tags...");
            if let Err(e) = untag_self_as_worker(&ec2_client, &current_instance_id).await {
                warn!("⚠️  Failed to untag self: {}", e);
            }

            vllm.stop().await?;
            info!("✅ Shutdown complete");
        }
        result = &mut monitor_task => {
            info!("Monitor task ended: {:?}", result);

            // Untag self from cluster
            if let Err(e) = untag_self_as_worker(&ec2_client, &current_instance_id).await {
                warn!("⚠️  Failed to untag self: {}", e);
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

                match action.as_str() {
                    "checkpoint" => {
                        info!("📦 Checkpoint action not yet implemented");
                        // TODO: Implement checkpoint action
                    }
                    "log" | _ => {
                        info!("📝 Logged spot interruption notice");
                    }
                }
            }
            _ => debug!("Spot notice: {:?}", notice.action),
        }
    }

    Ok(())
}

/// Checkpoint a running container
async fn checkpoint_container(container_id: String, checkpoint_id: String) -> anyhow::Result<()> {
    info!("📦 Checkpointing container '{}' as '{}'", container_id, checkpoint_id);

    // Use Docker checkpoint command
    let output = std::process::Command::new("docker")
        .args(["checkpoint", "create", &container_id, &checkpoint_id])
        .output()?;

    if output.status.success() {
        info!("✅ Checkpoint created: {}", checkpoint_id);
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Checkpoint failed: {}", stderr);
    }
}

/// Restore a container from checkpoint
async fn restore_container(checkpoint_id: String, container_name: String) -> anyhow::Result<()> {
    info!("📦 Restoring from checkpoint '{}' as '{}'", checkpoint_id, container_name);

    // TODO: Implement restore logic
    anyhow::bail!("Restore not yet implemented");
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

    let client = reqwest::Client::new();

    // IMDSv2: Get token first
    let token = client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "60")
        .send()
        .await?
        .text()
        .await?;

    // Helper to get metadata
    async fn get_metadata(
        client: &reqwest::Client,
        token: &str,
        path: &str,
    ) -> anyhow::Result<String> {
        let response = client
            .get(&format!("http://169.254.169.254/latest/meta-data/{}", path))
            .header("X-aws-ec2-metadata-token", token)
            .send()
            .await?;
        response.error_for_status_ref()?;
        response.text().await.map_err(Into::into)
    }

    let id = get_metadata(&client, &token, "instance-id").await?;
    let instance_type = get_metadata(&client, &token, "instance-type").await?;
    let public_ip = get_metadata(&client, &token, "public-ipv4").await.ok();
    let private_ip = get_metadata(&client, &token, "local-ipv4").await?;

    // Estimate GPU memory based on instance type
    let gpu_memory_gb = estimate_gpu_memory(&instance_type);

    Ok(Ec2Instance {
        id,
        instance_type,
        state: synkti_orchestrator::instance::InstanceState::Running,
        public_ip: if public_ip.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
            None
        } else {
            public_ip
        },
        private_ip: Some(private_ip),
        launch_time: chrono::Utc::now(),
        gpu_memory_gb,
        network_bandwidth_gbps: 10.0,
        gpu_memory_used_mb: 0.0,
        tags: HashMap::new(),
    })
}

/// Estimate GPU memory based on instance type
fn estimate_gpu_memory(instance_type: &str) -> f64 {
    match instance_type {
        t if t.starts_with("g4dn") => 16.0,
        t if t.starts_with("g5") => 24.0,
        t if t.starts_with("g6") => 24.0,
        t if t.starts_with("p3.2") => 16.0,
        t if t.starts_with("p3.8") => 64.0,
        t if t.starts_with("p3.16") => 128.0,
        t if t.starts_with("p3dn") => 256.0,
        t if t.starts_with("p4d") => 320.0,
        t if t.starts_with("p4de") => 640.0,
        t if t.starts_with("p5") => 640.0,
        _ => 16.0,
    }
}
