#!/bin/bash
# Synkti Control Plane User Data
# This script runs automatically on first boot
# Template variables: ${project_name}, ${models_bucket}, ${region}, ${synkti_binary_s3_path}, ${model_s3_path}, ${huggingface_model}

set -eux

# Configuration from Terraform
PROJECT_NAME="${project_name}"
MODELS_BUCKET="${models_bucket}"
REGION="${region}"
SYNKTI_BINARY_S3="${synkti_binary_s3_path}"
MODEL_S3_PATH="${model_s3_path}"
HUGGINGFACE_MODEL="${huggingface_model}"

# Log everything
exec > >(tee /var/log/synkti-user-data.log) 2>&1
echo "=== Synkti Control Plane Setup ==="
echo "Project: $PROJECT_NAME"
echo "Models Bucket: $MODELS_BUCKET"
echo "Region: $REGION"
echo "Synkti Binary: $SYNKTI_BINARY_S3"
echo "Model S3: $MODEL_S3_PATH"
echo ""

# Update system
echo "Updating system..."
yum update -y

# Install Docker
echo "Installing Docker..."
yum install -y docker
systemctl start docker
systemctl enable docker

# Install AWS CLI (for S3 operations)
echo "Installing AWS CLI..."
yum install -y aws-cli

# Install ec2-metadata tool for getting instance info
echo "Installing ec2-metadata..."
curl -o /usr/local/bin/ec2-metadata http://s3.amazonaws.com/ec2-metadata/ec2-metadata
chmod +x /usr/local/bin/ec2-metadata

# Download and install Synkti orchestrator from S3
echo "Downloading Synkti orchestrator from S3..."
if aws s3 cp "$SYNKTI_BINARY_S3" /usr/local/bin/synkti-orchestrator --region "$REGION"; then
  chmod +x /usr/local/bin/synkti-orchestrator
  echo "✅ Synkti orchestrator installed"
else
  echo "⚠️  Failed to download Synkti binary from S3"
  echo "   Make sure you've uploaded the binary to: $SYNKTI_BINARY_S3"
  echo "   Run: ./scripts/bootstrap.sh"
fi

# Create systemd service for auto-start
echo "Creating systemd service..."
cat > /etc/systemd/system/synkti.service <<'SERVICE'
[Unit]
Description=Synkti Orchestrator
After=docker.service network.target
Wants=docker.service

[Service]
Type=simple
User=root
Environment="PROJECT_NAME=$${PROJECT_NAME}"
Environment="MODELS_BUCKET=$${MODELS_BUCKET}"
Environment="REGION=$${REGION}"
Environment="MODEL_S3=$${MODEL_S3_PATH}"
Environment="HF_MODEL=$${HUGGINGFACE_MODEL}"

# Run synkti orchestrator with S3 model
ExecStart=/usr/local/bin/synkti-orchestrator run \\
    --model $${HUGGINGFACE_MODEL} \\
    --model-s3 $${MODEL_S3_PATH} \\
    --auto-infra \\
    --project $${PROJECT_NAME}

Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
SERVICE

# Enable and start service
echo "Enabling and starting Synkti service..."
systemctl daemon-reload
systemctl enable synkti.service
systemctl start synkti.service

# Tag instance so we know it's configured
instance_id=$(ec2-metadata -t | cut -d " " -f 2)
aws ec2 create-tags \
    --resource "$instance_id" \
    --tags Key=SynktiStatus,Value=Running \
    --region "$REGION" || true

echo ""
echo "=== Synkti Control Plane Setup Complete ==="
echo "Service status:"
systemctl status synkti.service --no-pager || true
echo ""
echo "View logs with: journalctl -u synkti -f"
