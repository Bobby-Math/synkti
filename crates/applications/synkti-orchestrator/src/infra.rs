//! Terraform infrastructure management module.
//!
//! Provides RAII-style infrastructure management by wrapping Terraform commands.
//! Infrastructure is created on demand and automatically cleaned up on exit.

use std::process::Command;
use anyhow::{Result, Context};

/// Terraform runner that wraps terraform CLI commands.
pub struct TerraformRunner {
    /// Path to the directory containing Terraform configuration
    pub infra_dir: String,
    /// Project name for resource naming
    pub project_name: String,
}

impl TerraformRunner {
    /// Create a new Terraform runner.
    pub fn new(infra_dir: &str, project_name: &str) -> Self {
        Self {
            infra_dir: infra_dir.to_string(),
            project_name: project_name.to_string(),
        }
    }

    /// Initialize Terraform (terraform init).
    pub fn init(&self) -> Result<()> {
        info!("Running terraform init in {}", self.infra_dir);

        let output = Command::new("terraform")
            .args(["init"])
            .current_dir(&self.infra_dir)
            .output()?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("terraform init failed: {}", stderr);
        }
    }

    /// Apply Terraform configuration (terraform apply).
    pub fn apply(&self) -> Result<TerraformOutputs> {
        info!("Applying Terraform configuration for project: {}", self.project_name);

        let output = Command::new("terraform")
            .args([
                "apply",
                "-auto-approve",
                &format!("-var=project_name={}", self.project_name),
            ])
            .current_dir(&self.infra_dir)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("terraform apply failed: {}", stderr);
        }

        self.parse_outputs()
    }

    /// Destroy Terraform configuration (terraform destroy).
    pub fn destroy(&self) -> Result<()> {
        info!("Destroying Terraform configuration for project: {}", self.project_name);

        let output = Command::new("terraform")
            .args([
                "destroy",
                "-auto-approve",
                &format!("-var=project_name={}", self.project_name),
            ])
            .current_dir(&self.infra_dir)
            .output()?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("terraform destroy failed: {}", stderr);
        }
    }

    /// Get terraform output value by name.
    pub fn get_output(&self, name: &str) -> Result<String> {
        let output = Command::new("terraform")
            .args(["output", "-raw", name])
            .current_dir(&self.infra_dir)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("failed to get output {}: {}", name, stderr);
        }
    }

    /// Parse all terraform outputs into structured data.
    pub fn parse_outputs(&self) -> Result<TerraformOutputs> {
        Ok(TerraformOutputs {
            control_plane_instance_ids: self.get_output("control_plane_instance_ids")?,
            control_plane_public_ips: self.get_output("control_plane_public_ips")?,
            worker_instance_profile_name: self.get_output("worker_instance_profile_name")?,
            worker_sg_id: self.get_output("worker_sg_id")?,
            checkpoint_bucket_name: self.get_output("checkpoint_bucket_name")?,
            models_bucket_name: self.get_output("models_bucket_name")?,
            connect_command: self.get_output("connect_to_control_plane")?,
            launch_command: self.get_output("launch_worker_command")?,
        })
    }

    /// Show infrastructure status by parsing outputs.
    pub fn status(&self) -> Result<InfraStatus> {
        let instance_ids = self.get_output("control_plane_instance_ids")?;
        let public_ips = self.get_output("control_plane_public_ips")?;
        let worker_role = self.get_output("worker_instance_profile_name")?;
        let sg_id = self.get_output("worker_sg_id")?;
        let bucket = self.get_output("checkpoint_bucket_name")?;

        Ok(InfraStatus {
            project_name: self.project_name.clone(),
            control_plane_instance_ids: instance_ids.lines().map(|s| s.to_string()).collect(),
            control_plane_public_ips: public_ips.lines().map(|s| s.to_string()).collect(),
            worker_instance_profile_name: worker_role,
            worker_sg_id: sg_id,
            checkpoint_bucket_name: bucket,
            models_bucket_name: self.get_output("models_bucket_name")?,
        })
    }
}

/// Terraform output values.
#[derive(Debug, Clone)]
pub struct TerraformOutputs {
    pub control_plane_instance_ids: String,
    pub control_plane_public_ips: String,
    pub worker_instance_profile_name: String,
    pub worker_sg_id: String,
    pub checkpoint_bucket_name: String,
    pub models_bucket_name: String,
    pub connect_command: String,
    pub launch_command: String,
}

/// Infrastructure status information.
#[derive(Debug, Clone)]
pub struct InfraStatus {
    pub project_name: String,
    pub control_plane_instance_ids: Vec<String>,
    pub control_plane_public_ips: Vec<String>,
    pub worker_instance_profile_name: String,
    pub worker_sg_id: String,
    pub checkpoint_bucket_name: String,
    pub models_bucket_name: String,
}

/// Create a marker file to track that this orchestrator owns the infrastructure.
pub fn create_owner_marker(project_name: &str) -> Result<()> {
    let marker_path = format!("/tmp/synkti-{}.owner", project_name);
    std::fs::write(&marker_path, std::process::id().to_string())
        .context("failed to create owner marker")?;
    Ok(())
}

/// Remove the owner marker file.
pub fn remove_owner_marker(project_name: &str) -> Result<()> {
    let marker_path = format!("/tmp/synkti-{}.owner", project_name);
    std::fs::remove_file(&marker_path).ok();
    Ok(())
}

/// Check if this process is the owner of the infrastructure.
pub fn is_owner(project_name: &str) -> bool {
    let marker_path = format!("/tmp/synkti-{}.owner", project_name);
    if let Ok(content) = std::fs::read_to_string(&marker_path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            return pid == std::process::id();
        }
    }
    false
}

/// Check if the infrastructure has a stale owner (process no longer running).
pub fn has_stale_owner(project_name: &str) -> bool {
    let marker_path = format!("/tmp/synkti-{}.owner", project_name);
    if let Ok(content) = std::fs::read_to_string(&marker_path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            // Try to check if process exists by sending signal 0
            // On Linux, we can check /proc
            if std::path::Path::new(&format!("/proc/{}", pid)).exists() {
                // Process is still running
                return false;
            } else {
                // Process no longer exists, stale marker
                return true;
            }
        }
    }
    false
}

/// Clean up stale owner marker.
pub fn cleanup_stale_owner(project_name: &str) -> Result<()> {
    if has_stale_owner(project_name) {
        let marker_path = format!("/tmp/synkti-{}.owner", project_name);
        std::fs::remove_file(&marker_path).ok();
    }
    Ok(())
}

use tracing::info;
