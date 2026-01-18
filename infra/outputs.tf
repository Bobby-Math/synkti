# Output values for Synkti infrastructure

output "control_plane_instance_ids" {
  description = "Control plane instance IDs"
  value       = aws_instance.control_plane[*].id
}

output "control_plane_public_ips" {
  description = "Control plane public IPs"
  value       = aws_instance.control_plane[*].public_ip
}

output "worker_instance_ids" {
  description = "GPU worker instance IDs (created via Terraform)"
  value       = aws_instance.gpu_worker[*].id
}

output "worker_role_name" {
  description = "Worker IAM role name (use with synkti launch --iam-profile)"
  value       = aws_iam_role.worker.name
}

output "worker_instance_profile_name" {
  description = "Worker instance profile name (use with synkti launch --iam-profile)"
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

output "control_plane_sg_id" {
  description = "Control plane security group ID"
  value       = aws_security_group.control_plane.id
}

output "worker_sg_id" {
  description = "Worker security group ID (use with synkti launch --security-group)"
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

# --- Usage Instructions ---

output "connect_to_control_plane" {
  description = "Command to connect to control plane via SSM"
  value       = length(aws_instance.control_plane) > 0 ? "aws ssm start-session --target ${aws_instance.control_plane[0].id} --region ${var.aws_region}" : "No control plane instances"
}

output "launch_worker_command" {
  description = "Example synkti launch command using Terraformed resources"
  value       = <<-EOT
    synkti-orchestrator launch \
      --ami ami-xxx \
      --instance-type g4dn.xlarge \
      --iam-profile ${aws_iam_instance_profile.worker.name} \
      --security-group ${aws_security_group.worker.id} \
      --name gpu-worker-1
    EOT
}

output "upload_model_command" {
  description = "Command to upload model weights to permanent storage"
  value       = <<-EOT
    # Upload model from HuggingFace to permanent S3 storage
    # 1. Download model (requires huggingface-cli login for gated models)
    git clone https://huggingface.co/META/LLAMA-MODEL /tmp/model
    # 2. Upload to S3
    aws s3 sync /tmp/model s3://${aws_s3_bucket.models.id}/llama-model/
    # 3. Use with synkti run:
    #    synkti-orchestrator run --model-s3 s3://${aws_s3_bucket.models.id}/llama-model/ ...
    EOT
}
