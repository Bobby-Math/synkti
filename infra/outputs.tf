# Output values for Synkti infrastructure (P2P Architecture)

output "worker_role_name" {
  description = "Worker IAM role name"
  value       = aws_iam_role.worker.name
}

output "worker_instance_profile_name" {
  description = "Worker instance profile name"
  value       = aws_iam_instance_profile.worker.name
}

output "checkpoint_bucket_name" {
  description = "S3 checkpoint bucket name (ephemeral)"
  value       = aws_s3_bucket.checkpoints.id
}

output "models_bucket_name" {
  description = "S3 models bucket name (PERMANENT - not deleted on destroy)"
  value       = aws_s3_bucket.models.id
}

output "worker_sg_id" {
  description = "Worker security group ID"
  value       = aws_security_group.worker.id
}

output "vpc_id" {
  description = "VPC ID"
  value       = data.aws_vpc.default.id
}

output "region" {
  description = "AWS region"
  value       = var.aws_region
}

output "launch_worker_command" {
  description = "Command to launch a worker instance via synkti CLI"
  value       = <<-EOT
    # Launch workers via synkti CLI (AWS SDK - no terraform state issues)
    synkti --project-name ${var.project_name} worker launch --instance-type ${var.worker_instance_type}
    synkti --project-name ${var.project_name} worker list
    synkti --project-name ${var.project_name} worker terminate <instance-id>
    EOT
}

output "upload_model_command" {
  description = "Command to upload model weights to permanent storage"
  value       = <<-EOT
    # Upload model from HuggingFace to permanent S3 storage
    ./scripts/upload-model.sh --project-name ${var.project_name} --model Qwen/Qwen2.5-7B-Instruct
    EOT
}
