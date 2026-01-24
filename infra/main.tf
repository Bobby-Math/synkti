# Synkti Infrastructure - Terraform Configuration
# P2P Architecture: No control plane - all nodes are self-governing workers
# RAII-style infrastructure: terraform apply creates, terraform destroy cleans up

terraform {
  required_version = ">= 1.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }

  backend "local" {
    path = "./terraform.tfstate"
  }
}

# Provider
provider "aws" {
  region = var.aws_region
}

# --- Data Sources ---

# Get latest Amazon Linux 2023 AMI (has OpenSSL 3.x for synkti binary compatibility)
data "aws_ami" "al2023" {
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["al2023-ami-2023.*-x86_64"]
  }

  filter {
    name   = "virtualization-type"
    values = ["hvm"]
  }
}

# Get ECS-optimized GPU AMI from SSM (has NVIDIA drivers for G4/G5 instances)
# Uses SSM parameter which is more reliable than AMI name filtering
data "aws_ssm_parameter" "ecs_gpu_ami" {
  name = "/aws/service/ecs/optimized-ami/amazon-linux-2023/gpu/recommended/image_id"
}

# Get default VPC
data "aws_vpc" "default" {
  default = true
}

# --- IAM Roles ---

# Worker Role (all nodes are workers in P2P architecture)
resource "aws_iam_role" "worker" {
  name = "${var.project_name}-worker"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action = "sts:AssumeRole"
      Effect = "Allow"
      Principal = {
        Service = "ec2.amazonaws.com"
      }
    }]
  })

  tags = {
    Name      = "${var.project_name}-worker"
    ManagedBy = "Synkti"
    Project   = var.project_name
    Lifecycle = "Permanent"
  }

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_iam_role_policy_attachment" "worker_ssm" {
  role       = aws_iam_role.worker.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_role_policy_attachment" "worker_s3_read" {
  role       = aws_iam_role.worker.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess"
}

resource "aws_iam_role_policy_attachment" "worker_ec2" {
  role       = aws_iam_role.worker.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEC2FullAccess"
}

resource "aws_iam_role_policy_attachment" "worker_s3_full" {
  role       = aws_iam_role.worker.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonS3FullAccess"
}

# Explicit policy for models bucket access
resource "aws_iam_role_policy" "worker_models_read" {
  name = "${var.project_name}-worker-models-read"
  role = aws_iam_role.worker.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "s3:GetObject",
          "s3:ListBucket",
          "s3:PutObject",
          "s3:DeleteObject"
        ]
        Resource = [
          aws_s3_bucket.models.arn,
          "${aws_s3_bucket.models.arn}/*"
        ]
      }
    ]
  })
}

resource "aws_iam_instance_profile" "worker" {
  name = "${var.project_name}-worker"
  role = aws_iam_role.worker.name

  lifecycle {
    prevent_destroy = true
  }
}

# --- S3 Buckets ---

# Permanent Models Bucket (NOT deleted on destroy)
resource "aws_s3_bucket" "models" {
  bucket = "${var.project_name}-models"

  tags = {
    Name      = "${var.project_name}-models"
    ManagedBy  = "Synkti"
    Project   = var.project_name
    Lifecycle = "Permanent"
  }

  lifecycle {
    prevent_destroy = true
  }
}

# Versioning for models bucket (protects against accidental deletion)
resource "aws_s3_bucket_versioning" "models" {
  bucket = aws_s3_bucket.models.id

  versioning_configuration {
    status = "Enabled"
  }
}

# Server-side encryption for models
resource "aws_s3_bucket_server_side_encryption_configuration" "models" {
  bucket = aws_s3_bucket.models.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

# Lifecycle: Transition to cold storage after 90 days for cost savings
resource "aws_s3_bucket_lifecycle_configuration" "models" {
  bucket = aws_s3_bucket.models.id

  rule {
    id     = "transition-to-glacier"
    status = "Enabled"

    filter {
      prefix = ""
    }

    transition {
      days          = 90
      storage_class = "GLACIER_IR"
    }
  }
}

# Checkpoints Bucket (ephemeral - deleted on destroy)
resource "aws_s3_bucket" "checkpoints" {
  bucket_prefix = "${var.project_name}-checkpoints-"

  tags = {
    Name      = "${var.project_name}-checkpoints"
    ManagedBy = "Synkti"
    Project   = var.project_name
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "checkpoints" {
  bucket = aws_s3_bucket.checkpoints.id

  rule {
    id     = "delete-old"
    status = "Enabled"

    filter {
      prefix = ""
    }

    expiration {
      days = var.checkpoint_bucket_expiration_days
    }
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "checkpoints" {
  bucket = aws_s3_bucket.checkpoints.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

# --- Security Groups ---

# Worker SG (all nodes use this in P2P architecture)
resource "aws_security_group" "worker" {
  name        = "${var.project_name}-worker"
  description = "Synkti worker security group (P2P - all nodes are workers)"
  vpc_id      = data.aws_vpc.default.id

  tags = {
    Name      = "${var.project_name}-worker"
    ManagedBy = "Synkti"
    Project   = var.project_name
    Lifecycle = "Permanent"
  }

  lifecycle {
    prevent_destroy = true
  }
}

# Allow SSH from allowed CIDR blocks
resource "aws_vpc_security_group_ingress_rule" "worker_ssh" {
  for_each       = toset(var.allowed_cidr_blocks)
  security_group_id = aws_security_group.worker.id
  description         = "SSH from ${each.value}"
  ip_protocol         = "tcp"
  from_port           = 22
  to_port             = 22
  cidr_ipv4           = each.value
}

# Allow HTTP/HTTPS from allowed CIDR blocks (for vLLM API access)
resource "aws_vpc_security_group_ingress_rule" "worker_http" {
  for_each       = toset(var.allowed_cidr_blocks)
  security_group_id = aws_security_group.worker.id
  description        = "HTTP from ${each.value}"
  ip_protocol        = "tcp"
  from_port          = 80
  to_port            = 80
  cidr_ipv4          = each.value
}

resource "aws_vpc_security_group_ingress_rule" "worker_https" {
  for_each       = toset(var.allowed_cidr_blocks)
  security_group_id = aws_security_group.worker.id
  description        = "HTTPS from ${each.value}"
  ip_protocol        = "tcp"
  from_port          = 443
  to_port            = 443
  cidr_ipv4          = each.value
}

# Allow vLLM default port (8000) from allowed CIDR blocks
resource "aws_vpc_security_group_ingress_rule" "worker_vllm" {
  for_each       = toset(var.allowed_cidr_blocks)
  security_group_id = aws_security_group.worker.id
  description        = "vLLM API from ${each.value}"
  ip_protocol        = "tcp"
  from_port          = 8000
  to_port            = 8000
  cidr_ipv4          = each.value
}

# Allow P2P communication between workers
resource "aws_vpc_security_group_ingress_rule" "worker_p2p" {
  security_group_id              = aws_security_group.worker.id
  description                    = "P2P communication between workers"
  ip_protocol                    = "-1"
  referenced_security_group_id  = aws_security_group.worker.id
}

# Allow all outbound traffic (required for S3, SSM, yum, etc.)
resource "aws_vpc_security_group_egress_rule" "worker_all_outbound" {
  security_group_id = aws_security_group.worker.id
  description       = "Allow all outbound traffic"
  ip_protocol       = "-1"
  cidr_ipv4         = "0.0.0.0/0"
}

# --- EC2 Instances ---

# Note: Spot instances are now managed via the synkti CLI using AWS SDK
# Use: synkti worker launch --project-name <project> --instance-type g4dn.xlarge
# This avoids terraform state issues with spot instances (can't be stopped)
