# Input variables for Synkti infrastructure

variable "aws_region" {
  description = "AWS region"
  type        = string
  default     = "us-east-1"
}

variable "project_name" {
  description = "Project name (used for resource naming)"
  type        = string
  default     = "synkti"

  validation {
    condition     = can(regex("^[a-z0-9-]+$", var.project_name))
    error_message = "Project name must be lowercase alphanumeric with hyphens only."
  }
}

variable "control_plane_count" {
  description = "Number of control plane instances"
  type        = number
  default     = 1

  validation {
    condition     = var.control_plane_count >= 0 && var.control_plane_count <= 10
    error_message = "Control plane count must be between 0 and 10."
  }
}

variable "control_plane_instance_type" {
  description = "Control plane instance type"
  type        = string
  default     = "t3.medium"

  validation {
    condition     = can(regex("^[a-z][0-9]\\.?((nano|micro|small|medium|large)|[0-9]+xlarge)$", var.control_plane_instance_type))
    error_message = "Must be a valid AWS instance type (e.g., t3.medium, t3.large)."
  }
}

variable "control_plane_ami_id" {
  description = "Custom control plane AMI (empty to use Amazon Linux 2 GPU AMI from SSM)"
  type        = string
  default     = ""
}

variable "worker_count" {
  description = "Number of GPU worker instances to create via Terraform (use 0 to launch via synkti CLI)"
  type        = number
  default     = 0

  validation {
    condition     = var.worker_count >= 0
    error_message = "Worker count must be non-negative."
  }
}

variable "worker_instance_type" {
  description = "Worker instance type"
  type        = string
  default     = "g4dn.xlarge"

  validation {
    condition     = can(regex("^(g[0-9]+|[p][0-9]+)", var.worker_instance_type))
    error_message = "Worker instance type should be a GPU instance (e.g., g4dn.xlarge, g5.xlarge, p3.2xlarge)."
  }
}

variable "worker_ami_id" {
  description = "Custom worker AMI (empty to use Amazon Linux 2 GPU AMI from SSM)"
  type        = string
  default     = ""
}

variable "allowed_cidr_blocks" {
  description = "CIDR blocks allowed to access control plane (SSH/HTTP)"
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "checkpoint_bucket_expiration_days" {
  description = "Number of days before checkpoint objects are deleted"
  type        = number
  default     = 7

  validation {
    condition     = var.checkpoint_bucket_expiration_days >= 1
    error_message = "Expiration days must be at least 1."
  }
}

variable "synkti_binary_s3_path" {
  description = "S3 path to synkti-orchestrator binary (for GitOps auto-install)"
  type        = string
  default     = ""
  # Example: "s3://my-project-models/bin/synkti-orchestrator"
}

variable "model_s3_path" {
  description = "S3 path to model weights (for auto-download)"
  type        = string
  default     = ""
  # Example: "s3://my-project-models/llama-2-7b/"
}

variable "huggingface_model_id" {
  description = "HuggingFace model ID (used if model_s3_path is empty)"
  type        = string
  default     = "meta-llama/Llama-2-7b-hf"
}
