# Synkti Infrastructure - Terraform Configuration
# RAII-style infrastructure: terraform apply creates, terraform destroy cleans up

terraform {
  required_version = ">= 1.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.0"
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

provider "random" {}

# --- Data Sources ---

# Get latest Amazon Linux 2 GPU AMI
data "aws_ssm_parameter" "gpu_ami" {
  name = "/aws/service/ecs/optimized-ami/amazon-linux-2/gpu/recommended/image_id"
}

# Get default VPC
data "aws_vpc" "default" {
  default = true
}

# --- IAM Roles ---

# Control Plane Role
resource "aws_iam_role" "control_plane" {
  name = "${var.project_name}-control-plane"

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
    Name      = "${var.project_name}-control-plane"
    ManagedBy = "Synkti"
    Project   = var.project_name
  }
}

resource "aws_iam_role_policy_attachment" "control_plane_ec2" {
  role       = aws_iam_role.control_plane.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEC2FullAccess"
}

resource "aws_iam_role_policy_attachment" "control_plane_s3" {
  role       = aws_iam_role.control_plane.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonS3FullAccess"
}

resource "aws_iam_role_policy_attachment" "control_plane_ssm" {
  role       = aws_iam_role.control_plane.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_role_policy" "control_plane_pass_role" {
  name = "${var.project_name}-pass-worker-role"
  role = aws_iam_role.control_plane.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action = "iam:PassRole"
      Effect = "Allow"
      Resource = aws_iam_role.worker.arn
    }]
  })
}

# Worker Role
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
          "s3:ListBucket"
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
}

# --- S3 Buckets ---

# Permanent Models Bucket (NOT deleted on destroy)
resource "aws_s3_bucket" "models" {
  bucket = "${var.project_name}-models"

  tags = {
    Name      = "${var.project_name}-models"
    ManagedBy = "Synkti"
    Project   = var.project_name
    Lifecycle = "Permanent"
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
      days = 7
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

resource "random_id" "suffix" {
  byte_length = 4
}

# --- Security Groups ---

# Control Plane SG
resource "aws_security_group" "control_plane" {
  name        = "${var.project_name}-control-plane"
  description = "Synkti control plane security group"
  vpc_id      = data.aws_vpc.default.id

  tags = {
    Name      = "${var.project_name}-control-plane"
    ManagedBy = "Synkti"
    Project   = var.project_name
  }
}

resource "aws_vpc_security_group_ingress_rule" "control_plane_ssh" {
  for_each = toset(var.allowed_cidr_blocks)
  security_group_id = aws_security_group.control_plane.id
  description        = "SSH from ${each.value}"
  ip_protocol      = "tcp"
  from_port         = 22
  to_port           = 22
  cidr_ipv4         = each.value
}

# Worker SG
resource "aws_security_group" "worker" {
  name        = "${var.project_name}-worker"
  description = "Synkti worker security group"
  vpc_id      = data.aws_vpc.default.id

  tags = {
    Name      = "${var.project_name}-worker"
    ManagedBy = "Synkti"
    Project   = var.project_name
  }
}

resource "aws_vpc_security_group_ingress_rule" "worker_from_control" {
  security_group_id      = aws_security_group.worker.id
  description             = "From control plane"
  ip_protocol            = "-1"
  referenced_security_group_id = aws_security_group.control_plane.id
}

# --- EC2 Instances ---

# Control Plane instance profile
resource "aws_iam_instance_profile" "control_plane" {
  name = "${var.project_name}-control-plane"
  role = aws_iam_role.control_plane.name
}

# Control Plane
resource "aws_instance" "control_plane" {
  count         = var.control_plane_count
  instance_type = var.control_plane_instance_type
  ami           = var.control_plane_ami_id != "" ? var.control_plane_ami_id : data.aws_ssm_parameter.gpu_ami.value

  iam_instance_profile = aws_iam_instance_profile.control_plane.name

  vpc_security_group_ids = [aws_security_group.control_plane.id]

  # Let AWS pick subnet in supported AZ
  # (t3.medium not available in us-east-1e)

  # GitOps: Auto-install and auto-start Synkti on boot
  # Build user_data script that uses variables
  user_data = templatefile("${path.module}/user-data.sh", {
    project_name           = var.project_name
    models_bucket          = aws_s3_bucket.models.id
    region                 = var.aws_region
    synkti_binary_s3_path = var.synkti_binary_s3_path != "" ? var.synkti_binary_s3_path : "s3://${aws_s3_bucket.models.id}/bin/synkti-orchestrator"
    model_s3_path          = var.model_s3_path != "" ? var.model_s3_path : "s3://${aws_s3_bucket.models.id}/llama-2-7b/"
    huggingface_model      = var.huggingface_model_id
  })

  tags = {
    Name      = "${var.project_name}-control-plane-${count.index}"
    ManagedBy = "Synkti"
    Project   = var.project_name
    Role      = "ControlPlane"
  }
}

# GPU Workers (optional - can also launch via synkti CLI)
resource "aws_instance" "gpu_worker" {
  count         = var.worker_count
  instance_type = var.worker_instance_type
  ami           = var.worker_ami_id != "" ? var.worker_ami_id : data.aws_ssm_parameter.gpu_ami.value

  iam_instance_profile = aws_iam_instance_profile.worker.name

  vpc_security_group_ids = [aws_security_group.worker.id]

  # Let AWS pick subnet in supported AZ

  user_data = <<-EOF
              #!/bin/bash
              set -eux
              yum update -y
              # NVIDIA drivers and CUDA installed via AMI or manual setup
              systemctl start docker
              systemctl enable docker
              EOF

  tags = {
    Name      = "${var.project_name}-worker-${count.index}"
    ManagedBy = "Synkti"
    Project   = var.project_name
    Role      = "Worker"
  }

  # Wait for SSM to be ready before considering this instance created
  depends_on = [
    aws_iam_role_policy_attachment.worker_ssm,
    aws_iam_role_policy_attachment.worker_s3_read,
  ]
}
