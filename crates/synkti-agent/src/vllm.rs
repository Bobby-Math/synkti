//! vLLM container management
//!
//! Manages vLLM Docker containers for ML inference.

use crate::error::{OrchestratorError, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;
use tokio::process::Command as AsyncCommand;
use tracing::{debug, error, info, warn};

/// vLLM configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VllmConfig {
    /// vLLM Docker image
    #[serde(default = "default_vllm_image")]
    pub image: String,

    /// Model to serve (HuggingFace model ID)
    pub model: String,

    /// Port for vLLM API
    #[serde(default = "default_port")]
    pub port: u16,

    /// Maximum context length
    #[serde(default = "default_max_model_len")]
    pub max_model_len: usize,

    /// Tensor parallel size (number of GPUs)
    #[serde(default = "default_tensor_parallel_size")]
    pub tensor_parallel_size: usize,

    /// Quantization format (awq, gptq, etc.)
    pub quantization: Option<String>,

    /// GPU memory utilization (fraction, 0.0-1.0)
    #[serde(default = "default_gpu_memory_utilization")]
    pub gpu_memory_utilization: f64,

    /// Host to bind to
    #[serde(default = "default_host")]
    pub host: String,

    /// Container name
    pub container_name: Option<String>,
}

fn default_vllm_image() -> String {
    "vllm/vllm-openai:latest".to_string()
}

fn default_port() -> u16 {
    8000
}

fn default_max_model_len() -> usize {
    4096
}

fn default_tensor_parallel_size() -> usize {
    1
}

fn default_gpu_memory_utilization() -> f64 {
    0.9
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

impl Default for VllmConfig {
    fn default() -> Self {
        Self {
            image: default_vllm_image(),
            model: "meta-llama/Llama-2-7b-hf".to_string(),
            port: default_port(),
            max_model_len: default_max_model_len(),
            tensor_parallel_size: default_tensor_parallel_size(),
            quantization: None,
            gpu_memory_utilization: default_gpu_memory_utilization(),
            host: default_host(),
            container_name: None,
        }
    }
}

impl VllmConfig {
    /// Create a new vLLM config
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    /// Set Docker image
    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    /// Set port
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Set max context length
    pub fn with_max_model_len(mut self, len: usize) -> Self {
        self.max_model_len = len;
        self
    }

    /// Set tensor parallel size
    pub fn with_tensor_parallel_size(mut self, size: usize) -> Self {
        self.tensor_parallel_size = size;
        self
    }

    /// Set quantization
    pub fn with_quantization(mut self, quantization: impl Into<String>) -> Self {
        self.quantization = Some(quantization.into());
        self
    }

    /// Set container name
    pub fn with_container_name(mut self, name: impl Into<String>) -> Self {
        self.container_name = Some(name.into());
        self
    }

    /// Check if GPU is available on this system
    fn has_gpu() -> bool {
        // Check for nvidia-smi or GPU devices
        std::path::Path::new("/dev/nvidia0").exists()
            || std::path::Path::new("/usr/bin/nvidia-smi").exists()
            || std::path::Path::new("/usr/local/bin/nvidia-smi").exists()
    }

    /// Build Docker run arguments
    fn docker_run_args(&self) -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "-p".to_string(),
            format!("{}:{}", self.port, self.port),
        ];

        // Mount the model directory into the container
        // The model path on host is mapped to the same path inside the container
        args.push("-v".to_string());
        args.push(format!("{}:{}", self.model, self.model));

        // Use nvidia runtime for GPU support (more compatible than --gpus all)
        if Self::has_gpu() {
            // Try to use nvidia runtime first, fall back to --gpus all
            args.push("--runtime".to_string());
            args.push("nvidia".to_string());
            // Set PyTorch to use expandable memory segments (reduces OOM during warmup)
            args.push("--env".to_string());
            args.push("PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True".to_string());
        } else {
            tracing::warn!("⚠️  No GPU detected, running in CPU mode (vLLM will be slow or may not work)");
        }

        if let Some(ref name) = self.container_name {
            args.push("--name".to_string());
            args.push(name.clone());
        }

        args.push(self.image.clone());
        args.push("--model".to_string());
        args.push(self.model.clone());
        args.push("--port".to_string());
        args.push(self.port.to_string());
        args.push("--max-model-len".to_string());
        args.push(self.max_model_len.to_string());
        args.push("--gpu-memory-utilization".to_string());
        args.push(self.gpu_memory_utilization.to_string());

        if self.tensor_parallel_size > 1 {
            args.push("--tensor-parallel-size".to_string());
            args.push(self.tensor_parallel_size.to_string());
        }

        if let Some(ref quant) = self.quantization {
            args.push("--quantization".to_string());
            args.push(quant.clone());
        }

        args
    }
}

/// vLLM container manager
pub struct VllmContainer {
    /// vLLM configuration
    config: VllmConfig,

    /// Container ID (if running)
    container_id: Option<String>,
}

impl VllmContainer {
    /// Create a new vLLM container manager
    pub fn new(config: VllmConfig) -> Self {
        Self {
            config,
            container_id: None,
        }
    }

    /// Start the vLLM container
    pub async fn start(&mut self) -> Result<String> {
        info!("🤖 Starting vLLM container for model {}", self.config.model);

        // Cold start timestamp tracking
        let _ = std::fs::write("/tmp/cold-start-vllm.log", &format!("timestamp={} phase=vllm_start\n", chrono::Utc::now().timestamp()));

        // Verify model directory exists before starting container
        if std::path::Path::new(&self.config.model).exists() {
            info!("✓ Model directory exists: {}", self.config.model);
            // List contents for verification
            if let Ok(entries) = std::fs::read_dir(&self.config.model) {
                let file_count = entries.count();
                info!("  Model directory contains {} files/directories", file_count);
            }
        } else {
            warn!("⚠️  Model directory not found: {}", self.config.model);
            warn!("   vLLM may fail to start - check model download completed");
        }

        // Remove any existing container with the same name (crash recovery)
        if let Some(ref name) = self.config.container_name {
            info!("Checking for existing container named '{}'...", name);
            let _ = AsyncCommand::new("docker")
                .args(["rm", "-f", name])
                .output()
                .await;
        }

        let args = self.config.docker_run_args();

        info!("Docker run command: docker {}", args.join(" "));

        let output = AsyncCommand::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| OrchestratorError::Docker(format!("Failed to start vLLM: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OrchestratorError::Docker(format!(
                "vLLM container failed to start: {}",
                stderr
            )));
        }

        let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        self.container_id = Some(container_id.clone());

        // Cold start timestamp: container started
        let _ = std::fs::write("/tmp/cold-start-vllm.log",
            &format!("timestamp={} phase=vllm_container_started container_id={}\n",
                    chrono::Utc::now().timestamp(), container_id));

        info!("vLLM container started: {}", container_id);

        // Wait for vLLM to be ready
        self.wait_for_ready().await?;

        Ok(container_id)
    }

    /// Wait for vLLM API to be ready
    async fn wait_for_ready(&self) -> Result<()> {
        let client = reqwest::Client::new();
        let health_url = format!("http://{}:{}/health", self.config.host, self.config.port);

        info!("⏳ Waiting for vLLM health endpoint at {}", health_url);

        // Wait up to 10 minutes (600 seconds) - large models can take time to load
        for i in 0..600 {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            match client.get(&health_url).send().await {
                Ok(response) if response.status().is_success() => {
                    info!("✅ vLLM API is ready");

                    // Cold start timestamp: vLLM ready!
                    let _ = std::fs::write("/tmp/cold-start-vllm-ready.log",
                        &format!("timestamp={} phase=vllm_health_ok\n", chrono::Utc::now().timestamp()));

                    // Also append to main cold start log
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("/tmp/cold-start.log")
                        .and_then(|mut f| {
                            use std::io::Write;
                            writeln!(f, "timestamp={} phase=vllm_health_ok", chrono::Utc::now().timestamp())
                        });

                    return Ok(());
                }
                Ok(response) => {
                    if (i + 1) % 30 == 0 {
                        info!("Waiting for vLLM to be ready... ({}/600) - status: {}", i + 1, response.status());
                    }
                }
                Err(e) => {
                    if (i + 1) % 30 == 0 {
                        debug!("Health check failed: {}", e);
                    }
                }
            }
        }

        // Health check failed - get diagnostic information
        error!("❌ vLLM did not become ready within 10 minutes");
        error!("   Health URL: {}", health_url);
        error!("   Container ID: {:?}", self.container_id);

        // Try to get container logs for diagnosis
        if let Some(ref container_id) = self.container_id {
            error!("📜 Fetching container logs for diagnosis...");

            let logs_output = tokio::process::Command::new("docker")
                .args(["logs", "--tail", "50", container_id])
                .output()
                .await;

            if let Ok(output) = logs_output {
                let logs = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!("--- vLLM Container Logs (last 50 lines) ---");
                for line in logs.lines().take(50) {
                    error!("   {}", line);
                }
                if !stderr.is_empty() {
                    error!("--- Container stderr ---");
                    for line in stderr.lines() {
                        error!("   {}", line);
                    }
                }
                error!("--- End of container logs ---");
            }

            // Check container status
            let inspect_output = tokio::process::Command::new("docker")
                .args(["inspect", "--format={{.State.Status}}", container_id])
                .output()
                .await;

            if let Ok(output) = inspect_output {
                let status = String::from_utf8_lossy(&output.stdout);
                error!("Container status: {}", status.trim());
            }

            // Check if GPU is accessible
            let gpu_output = tokio::process::Command::new("docker")
                .args(["exec", container_id, "nvidia-smi", "-L"])
                .output()
                .await;

            match gpu_output {
                Ok(output) if output.status.success() => {
                    let gpu_info = String::from_utf8_lossy(&output.stdout);
                    info!("GPU in container: {}", gpu_info.trim());
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    error!("GPU check failed: {}", stderr.trim());
                }
                Err(e) => {
                    error!("GPU check error: {}", e);
                }
            }

            // Check if model files are visible in container
            let model_output = tokio::process::Command::new("docker")
                .args(["exec", container_id, "ls", "-la", &self.config.model])
                .output()
                .await;

            if let Ok(output) = model_output {
                let ls_output = String::from_utf8_lossy(&output.stdout);
                if output.status.success() {
                    info!("Model directory contents:");
                    for line in ls_output.lines().take(10) {
                        info!("   {}", line);
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    error!("Model directory check failed: {}", stderr.trim());
                    error!("   Path: {}", self.config.model);
                }
            }
        }

        Err(OrchestratorError::Docker(
            "vLLM did not become ready within 10 minutes".to_string(),
        ))
    }

    /// Stop the vLLM container
    pub async fn stop(&self) -> Result<()> {
        if let Some(ref container_id) = self.container_id {
            info!("Stopping vLLM container {}", container_id);

            let output = AsyncCommand::new("docker")
                .args(["stop", container_id])
                .output()
                .await
                .map_err(|e| OrchestratorError::Docker(format!("Failed to stop container: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(OrchestratorError::Docker(format!(
                    "Failed to stop container: {}",
                    stderr
                )));
            }

            info!("vLLM container stopped");
        }

        Ok(())
    }

    /// Get container ID
    pub fn container_id(&self) -> Option<&str> {
        self.container_id.as_deref()
    }

    /// Get vLLM API base URL
    pub fn api_url(&self) -> String {
        format!("http://{}:{}", self.config.host, self.config.port)
    }

    /// Check if container is running
    pub async fn is_running(&self) -> bool {
        if let Some(ref container_id) = self.container_id {
            let output = Command::new("docker")
                .args(["inspect", "-f", "{{.State.Running}}", container_id])
                .output();

            if let Ok(o) = output {
                if o.status.success() {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    return stdout.trim() == "true";
                }
            }
        }
        false
    }

    /// Get container logs
    pub async fn logs(&self, tail: Option<u32>) -> Result<String> {
        let container_id = self
            .container_id
            .as_ref()
            .ok_or_else(|| OrchestratorError::Docker("Container not started".to_string()))?;

        let mut args = vec!["logs".to_string(), container_id.clone()];
        if let Some(tail_lines) = tail {
            args.push("--tail".to_string());
            args.push(tail_lines.to_string());
        }

        let output = AsyncCommand::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| OrchestratorError::Docker(format!("Failed to get logs: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OrchestratorError::Docker(format!("Failed to get logs: {}", stderr)));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Execute a checkpoint on the container
    pub async fn checkpoint(&self, checkpoint_id: &str) -> Result<()> {
        let container_id = self
            .container_id
            .as_ref()
            .ok_or_else(|| OrchestratorError::Docker("Container not started".to_string()))?;

        info!("Creating checkpoint {} for container {}", checkpoint_id, container_id);

        let output = AsyncCommand::new("docker")
            .args([
                "checkpoint",
                "create",
                "--checkpoint-dir=/tmp/checkpoints",
                "--leave=true",
                container_id,
                checkpoint_id,
            ])
            .output()
            .await
            .map_err(|e| OrchestratorError::Docker(format!("Failed to create checkpoint: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OrchestratorError::Docker(format!(
                "Checkpoint failed: {}",
                stderr
            )));
        }

        info!("Checkpoint {} created successfully", checkpoint_id);
        Ok(())
    }
}

/// vLLM API client for health checks and queries
pub struct VllmClient {
    /// Base URL for vLLM API
    base_url: String,

    /// HTTP client
    client: reqwest::Client,
}

impl VllmClient {
    /// Create a new vLLM API client
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Check if vLLM is healthy
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/health", self.base_url);

        match self.client.get(&url).send().await {
            Ok(response) => Ok(response.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    /// Get list of available models
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/v1/models", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(OrchestratorError::Http)?;

        if !response.status().is_success() {
            return Err(OrchestratorError::Docker(format!(
                "Failed to list models: status {}",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct ModelsResponse {
            data: Vec<ModelData>,
        }

        #[derive(Deserialize)]
        struct ModelData {
            id: String,
        }

        let models: ModelsResponse = response.json().await?;
        Ok(models.data.into_iter().map(|m| m.id).collect())
    }

    /// Get raw Prometheus metrics from vLLM
    ///
    /// vLLM exposes metrics at `/metrics` in Prometheus format.
    pub async fn get_metrics(&self) -> Result<String> {
        let url = format!("{}/metrics", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(OrchestratorError::Http)?;

        if !response.status().is_success() {
            return Err(OrchestratorError::Docker(format!(
                "Failed to get metrics: status {}",
                response.status()
            )));
        }

        Ok(response.text().await?)
    }

    /// Get the number of currently running requests
    ///
    /// Parses the `vllm:num_requests_running` metric from Prometheus output.
    /// Returns 0 if the metric is not found or cannot be parsed.
    pub async fn get_running_requests(&self) -> Result<u32> {
        let metrics = match self.get_metrics().await {
            Ok(m) => m,
            Err(_) => return Ok(0), // Assume no requests if we can't get metrics
        };

        // Parse Prometheus format: vllm:num_requests_running{...} VALUE
        // Also try: vllm_num_requests_running (older format)
        for line in metrics.lines() {
            let line = line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Check for running requests metric
            if line.starts_with("vllm:num_requests_running")
                || line.starts_with("vllm_num_requests_running")
            {
                // Extract the value (last space-separated token)
                if let Some(value_str) = line.split_whitespace().last() {
                    if let Ok(value) = value_str.parse::<f64>() {
                        return Ok(value as u32);
                    }
                }
            }
        }

        // Metric not found, assume no running requests
        Ok(0)
    }

    /// Get the number of waiting requests (queue depth)
    ///
    /// Parses the `vllm:num_requests_waiting` metric from Prometheus output.
    pub async fn get_waiting_requests(&self) -> Result<u32> {
        let metrics = match self.get_metrics().await {
            Ok(m) => m,
            Err(_) => return Ok(0),
        };

        for line in metrics.lines() {
            let line = line.trim();

            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if line.starts_with("vllm:num_requests_waiting")
                || line.starts_with("vllm_num_requests_waiting")
            {
                if let Some(value_str) = line.split_whitespace().last() {
                    if let Ok(value) = value_str.parse::<f64>() {
                        return Ok(value as u32);
                    }
                }
            }
        }

        Ok(0)
    }

    /// Check if the server is idle (no running or waiting requests)
    pub async fn is_idle(&self) -> Result<bool> {
        let running = self.get_running_requests().await?;
        let waiting = self.get_waiting_requests().await?;
        Ok(running == 0 && waiting == 0)
    }

    /// Get base URL
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vllm_config_builder() {
        let config = VllmConfig::new("meta-llama/Llama-2-7b-hf")
            .with_port(8080)
            .with_max_model_len(8192)
            .with_tensor_parallel_size(2)
            .with_quantization("awq")
            .with_container_name("vllm-test");

        assert_eq!(config.model, "meta-llama/Llama-2-7b-hf");
        assert_eq!(config.port, 8080);
        assert_eq!(config.max_model_len, 8192);
        assert_eq!(config.tensor_parallel_size, 2);
        assert_eq!(config.quantization, Some("awq".to_string()));
        assert_eq!(config.container_name, Some("vllm-test".to_string()));
    }

    #[test]
    fn test_vllm_config_serialization() {
        let config = VllmConfig {
            image: "vllm/vllm-openai:latest".to_string(),
            model: "meta-llama/Llama-2-7b-hf".to_string(),
            port: 8000,
            max_model_len: 4096,
            tensor_parallel_size: 1,
            quantization: Some("awq".to_string()),
            gpu_memory_utilization: 0.9,
            host: "0.0.0.0".to_string(),
            container_name: Some("vllm-server".to_string()),
        };

        let json = serde_json::to_string(&config).unwrap();
        let _parsed: VllmConfig = serde_json::from_str(&json).unwrap();
    }
}
