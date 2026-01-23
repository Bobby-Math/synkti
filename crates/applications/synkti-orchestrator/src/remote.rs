//! Remote execution via AWS SSM
//!
//! Executes commands on remote EC2 instances using AWS Systems Manager (SSM).
//! This is the preferred approach over SSH because:
//! - No SSH key management needed
//! - Uses IAM for authentication
//! - Works through NAT/firewalls
//! - Integrated with AWS security model
//!
//! ## Prerequisites
//!
//! Target instances must have:
//! - SSM Agent installed (Amazon Linux 2/2023 have it by default)
//! - IAM instance profile with `AmazonSSMManagedInstanceCore` policy
//! - Outbound HTTPS access to SSM endpoints

use crate::error::{OrchestratorError, Result};
use crate::vllm::VllmConfig;
use aws_sdk_ssm::types::CommandInvocationStatus;
use aws_sdk_ssm::Client as SsmClient;
use std::time::Duration;
use tracing::{debug, error, info};

/// Default timeout for SSM command execution
const DEFAULT_COMMAND_TIMEOUT_SECS: i32 = 600; // 10 minutes

/// Polling interval when waiting for command completion
const COMMAND_POLL_INTERVAL_MS: u64 = 2000;

/// Maximum time to wait for command completion
const MAX_WAIT_DURATION_SECS: u64 = 900; // 15 minutes

/// Remote executor using AWS SSM
pub struct SsmExecutor {
    client: SsmClient,
    /// Timeout for individual commands (seconds)
    command_timeout: i32,
}

impl SsmExecutor {
    /// Create a new SSM executor
    pub fn new(client: SsmClient) -> Self {
        Self {
            client,
            command_timeout: DEFAULT_COMMAND_TIMEOUT_SECS,
        }
    }

    /// Create SSM client from AWS config
    pub async fn from_config(config: &aws_config::SdkConfig) -> Self {
        let client = SsmClient::new(config);
        Self::new(client)
    }

    /// Set command timeout
    pub fn with_timeout(mut self, timeout_secs: i32) -> Self {
        self.command_timeout = timeout_secs;
        self
    }

    /// Execute a shell command on a remote instance
    ///
    /// Uses the AWS-RunShellScript document for Linux instances.
    ///
    /// # Arguments
    /// - `instance_id`: Target EC2 instance ID
    /// - `commands`: Shell commands to execute
    ///
    /// # Returns
    /// Command output on success
    pub async fn run_command(
        &self,
        instance_id: &str,
        commands: Vec<String>,
    ) -> Result<CommandResult> {
        info!(
            instance_id = %instance_id,
            commands = ?commands,
            "Sending SSM command"
        );

        let response = self
            .client
            .send_command()
            .instance_ids(instance_id)
            .document_name("AWS-RunShellScript")
            .parameters("commands", commands.clone())
            .timeout_seconds(self.command_timeout)
            .send()
            .await
            .map_err(|e| {
                OrchestratorError::Docker(format!("SSM send_command failed: {}", e))
            })?;

        let command = response.command().ok_or_else(|| {
            OrchestratorError::Docker("SSM response missing command".to_string())
        })?;

        let command_id = command.command_id().ok_or_else(|| {
            OrchestratorError::Docker("SSM response missing command_id".to_string())
        })?;

        info!(command_id = %command_id, "SSM command sent, waiting for completion");

        // Wait for command to complete
        self.wait_for_command(command_id, instance_id).await
    }

    /// Wait for a command to complete
    async fn wait_for_command(
        &self,
        command_id: &str,
        instance_id: &str,
    ) -> Result<CommandResult> {
        let start = std::time::Instant::now();
        let max_wait = Duration::from_secs(MAX_WAIT_DURATION_SECS);
        let poll_interval = Duration::from_millis(COMMAND_POLL_INTERVAL_MS);

        loop {
            if start.elapsed() > max_wait {
                return Err(OrchestratorError::Timeout(max_wait));
            }

            let response = self
                .client
                .get_command_invocation()
                .command_id(command_id)
                .instance_id(instance_id)
                .send()
                .await
                .map_err(|e| {
                    OrchestratorError::Docker(format!("SSM get_command_invocation failed: {}", e))
                })?;

            let status = response.status();

            match status {
                Some(CommandInvocationStatus::Success) => {
                    let stdout = response.standard_output_content().unwrap_or_default();
                    let stderr = response.standard_error_content().unwrap_or_default();

                    info!(
                        command_id = %command_id,
                        "SSM command completed successfully"
                    );

                    return Ok(CommandResult {
                        command_id: command_id.to_string(),
                        instance_id: instance_id.to_string(),
                        status: CommandStatus::Success,
                        stdout: stdout.to_string(),
                        stderr: stderr.to_string(),
                        exit_code: Some(response.response_code()),
                    });
                }
                Some(CommandInvocationStatus::Failed) => {
                    let stdout = response.standard_output_content().unwrap_or_default();
                    let stderr = response.standard_error_content().unwrap_or_default();

                    error!(
                        command_id = %command_id,
                        stderr = %stderr,
                        "SSM command failed"
                    );

                    return Ok(CommandResult {
                        command_id: command_id.to_string(),
                        instance_id: instance_id.to_string(),
                        status: CommandStatus::Failed,
                        stdout: stdout.to_string(),
                        stderr: stderr.to_string(),
                        exit_code: Some(response.response_code()),
                    });
                }
                Some(CommandInvocationStatus::Cancelled) => {
                    return Err(OrchestratorError::Docker(
                        "SSM command was cancelled".to_string(),
                    ));
                }
                Some(CommandInvocationStatus::TimedOut) => {
                    return Err(OrchestratorError::Timeout(Duration::from_secs(
                        self.command_timeout as u64,
                    )));
                }
                Some(CommandInvocationStatus::Pending)
                | Some(CommandInvocationStatus::InProgress) => {
                    debug!(
                        command_id = %command_id,
                        status = ?status,
                        "SSM command still running"
                    );
                }
                _ => {
                    debug!(
                        command_id = %command_id,
                        status = ?status,
                        "SSM command in unknown state"
                    );
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Start a vLLM container on a remote instance
    ///
    /// Generates and executes the docker run command via SSM.
    /// Detects GPU availability on the remote instance and includes GPU flags only if present.
    pub async fn start_vllm_container(
        &self,
        instance_id: &str,
        config: &VllmConfig,
    ) -> Result<CommandResult> {
        let container_name = config
            .container_name
            .clone()
            .unwrap_or_else(|| format!("vllm-{}", &instance_id[..8.min(instance_id.len())]));

        // Stop any existing container with the same name (ignore errors)
        let stop_cmd = format!("docker stop {} 2>/dev/null || true", container_name);
        let rm_cmd = format!("docker rm {} 2>/dev/null || true", container_name);

        // Check for GPU on remote instance first
        let gpu_check = "ls /dev/nvidia0 >/dev/null 2>&1 && echo 'gpu' || echo 'no-gpu'";

        // Build docker run command (GPU flags added conditionally)
        // We use a shell script that checks for GPU and adds --gpus all only if present
        let docker_script = format!(
            r#"if [ -e /dev/nvidia0 ] || command -v nvidia-smi >/dev/null 2>&1; then
  docker run -d --gpus all -p {port} --name {name} --env VLLM_USAGE={gpu_mem}% {image} --model {model} --port {port} --max-model-len {max_len} {extra_args}
else
  echo "Warning: No GPU detected, running in CPU mode" >&2
  docker run -d -p {port} --name {name} {image} --model {model} --port {port} --max-model-len {max_len} {extra_args}
fi"#,
            port = config.port,
            name = container_name,
            gpu_mem = (config.gpu_memory_utilization * 100.0) as i32,
            image = config.image,
            model = config.model,
            max_len = config.max_model_len,
            extra_args = {
                let mut extra = String::new();
                if config.tensor_parallel_size > 1 {
                    extra.push_str(&format!("--tensor-parallel-size {} ", config.tensor_parallel_size));
                }
                if let Some(ref quant) = config.quantization {
                    extra.push_str(&format!("--quantization {} ", quant));
                }
                extra
            }
        );

        let commands = vec![
            stop_cmd,
            rm_cmd,
            docker_script,
        ];

        info!(
            instance_id = %instance_id,
            container_name = %container_name,
            model = %config.model,
            "Starting vLLM container via SSM"
        );

        self.run_command(instance_id, commands).await
    }

    /// Stop a vLLM container on a remote instance
    pub async fn stop_vllm_container(
        &self,
        instance_id: &str,
        container_name: &str,
    ) -> Result<CommandResult> {
        let commands = vec![
            format!("docker stop {} || true", container_name),
            format!("docker rm {} || true", container_name),
        ];

        info!(
            instance_id = %instance_id,
            container_name = %container_name,
            "Stopping vLLM container via SSM"
        );

        self.run_command(instance_id, commands).await
    }

    /// Check if Docker is available on the instance
    pub async fn check_docker(&self, instance_id: &str) -> Result<bool> {
        let commands = vec!["docker --version".to_string()];

        match self.run_command(instance_id, commands).await {
            Ok(result) => Ok(result.status == CommandStatus::Success),
            Err(_) => Ok(false),
        }
    }

    /// Check if a container is running
    pub async fn is_container_running(
        &self,
        instance_id: &str,
        container_name: &str,
    ) -> Result<bool> {
        let commands = vec![format!(
            "docker inspect -f '{{{{.State.Running}}}}' {} 2>/dev/null || echo false",
            container_name
        )];

        match self.run_command(instance_id, commands).await {
            Ok(result) => Ok(result.stdout.trim() == "true"),
            Err(_) => Ok(false),
        }
    }

    /// Get container logs
    pub async fn get_container_logs(
        &self,
        instance_id: &str,
        container_name: &str,
        tail: Option<u32>,
    ) -> Result<String> {
        let tail_arg = tail.map(|n| format!("--tail {}", n)).unwrap_or_default();
        let commands = vec![format!("docker logs {} {}", tail_arg, container_name)];

        let result = self.run_command(instance_id, commands).await?;
        Ok(result.stdout)
    }
}

/// Result of an SSM command execution
#[derive(Debug, Clone)]
pub struct CommandResult {
    /// SSM command ID
    pub command_id: String,

    /// Target instance ID
    pub instance_id: String,

    /// Command status
    pub status: CommandStatus,

    /// Standard output
    pub stdout: String,

    /// Standard error
    pub stderr: String,

    /// Exit code (if available)
    pub exit_code: Option<i32>,
}

impl CommandResult {
    /// Check if command succeeded
    pub fn is_success(&self) -> bool {
        self.status == CommandStatus::Success
    }
}

/// Status of a command execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    /// Command completed successfully
    Success,
    /// Command failed
    Failed,
    /// Command is still running
    InProgress,
    /// Command was cancelled
    Cancelled,
    /// Command timed out
    TimedOut,
}

/// Create an SSM client from the default AWS config
pub async fn create_ssm_client() -> SsmClient {
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    SsmClient::new(&config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_result_is_success() {
        let result = CommandResult {
            command_id: "cmd-123".to_string(),
            instance_id: "i-abc".to_string(),
            status: CommandStatus::Success,
            stdout: "ok".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
        };

        assert!(result.is_success());
    }

    #[test]
    fn test_command_result_is_failure() {
        let result = CommandResult {
            command_id: "cmd-123".to_string(),
            instance_id: "i-abc".to_string(),
            status: CommandStatus::Failed,
            stdout: String::new(),
            stderr: "error".to_string(),
            exit_code: Some(1),
        };

        assert!(!result.is_success());
    }
}
