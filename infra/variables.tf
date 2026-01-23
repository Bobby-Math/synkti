# Input variables for Synkti infrastructure (P2P Architecture)

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

variable "worker_count" {
  description = "DEPRECATED: Workers are now launched via synkti CLI (AWS SDK) - see launch_worker_command output"
  type        = number
  default     = 0

  validation {
    condition     = var.worker_count >= 0
    error_message = "Worker count must be non-negative."
  }
}

variable "worker_instance_type" {
  description = "Worker instance type (GPU recommended: g4dn.xlarge, g5.xlarge; CPU for testing: t3.medium)"
  type        = string
  default     = "g4dn.xlarge"
}

variable "worker_ami_id" {
  description = "Custom worker AMI (empty to use Amazon Linux 2 GPU AMI from SSM)"
  type        = string
  default     = ""
}

variable "allowed_cidr_blocks" {
  description = "CIDR blocks allowed to access workers (SSH/HTTP/vLLM API)"
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
  description = "S3 path to synkti binary (for GitOps auto-install)"
  type        = string
  default     = ""
  # Example: "s3://my-project-models/bin/synkti"
}

variable "model_s3_path" {
  description = "S3 path to model weights (for auto-download)"
  type        = string
  default     = ""
  # Example: "s3://my-project-models/qwen2.5-7b/"
}

variable "huggingface_model_id" {
  description = "HuggingFace model ID (used if model_s3_path is empty)"
  type        = string
  default     = "Qwen/Qwen2.5-7B-Instruct"
}
