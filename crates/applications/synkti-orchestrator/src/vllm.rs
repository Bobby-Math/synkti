//! vLLM container management
//!
//! Manages vLLM Docker containers for ML inference.

use crate::error::{OrchestratorError, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;
use tokio::process::Command as AsyncCommand;
use tracing::{debug, info};

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

    /// Build Docker run arguments
    fn docker_run_args(&self) -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--gpus".to_string(),
            "all".to_string(),
            "-p".to_string(),
            format!("{}:{}", self.port, self.port),
            "--env".to_string(),
            format!("VLLM_USAGE={}%", self.gpu_memory_utilization * 100.0),
        ];

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
        info!("Starting vLLM container for model {}", self.config.model);

        let args = self.config.docker_run_args();

        debug!("Docker run command: {:?}", args);

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

        info!("vLLM container started: {}", container_id);

        // Wait for vLLM to be ready
        self.wait_for_ready().await?;

        Ok(container_id)
    }

    /// Wait for vLLM API to be ready
    async fn wait_for_ready(&self) -> Result<()> {
        let client = reqwest::Client::new();
        let health_url = format!("http://{}:{}/health", self.config.host, self.config.port);

        for i in 0..30 {
            // Wait up to 30 seconds
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            match client.get(&health_url).send().await {
                Ok(response) if response.status().is_success() => {
                    info!("vLLM API is ready");
                    return Ok(());
                }
                Ok(_) => {
                    debug!("Waiting for vLLM to be ready... ({}/30)", i + 1);
                }
                Err(e) => {
                    debug!("Health check failed: {}", e);
                }
            }
        }

        Err(OrchestratorError::Docker(
            "vLLM did not become ready within 30 seconds".to_string(),
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
