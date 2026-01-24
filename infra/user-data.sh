#!/bin/bash
# Synkti Worker User Data (P2P Architecture)
# All nodes are workers that self-govern
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
echo "=== Synkti Worker Setup (P2P Architecture) ==="
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

# Download and install Synkti from S3
echo "Downloading Synkti from S3..."
if aws s3 cp "$SYNKTI_BINARY_S3" /usr/local/bin/synkti --region "$REGION"; then
  chmod +x /usr/local/bin/synkti
  echo "✅ Synkti installed"
else
  echo "⚠️  Failed to download Synkti binary from S3"
  echo "   Make sure you've uploaded the binary to: $SYNKTI_BINARY_S3"
  echo "   Run: ./scripts/upload-binary.sh --project-name $PROJECT_NAME"
fi

# Pull vLLM image
echo "Pulling vLLM Docker image..."
docker pull vllm/vllm-openai:latest

# Create systemd service for auto-start
echo "Creating systemd service..."
cat > /etc/systemd/system/synkti.service <<EOF
[Unit]
Description=Synkti P2P Orchestrator
After=docker.service network.target
Wants=docker.service

[Service]
Type=simple
User=root
Environment="PROJECT_NAME=${project_name}"
Environment="REGION=${region}"
Environment="MODELS_BUCKET=${models_bucket}"
Environment="MODEL_S3_PATH=${model_s3_path}"
Environment="HUGGINGFACE_MODEL=${huggingface_model}"
# RAII is ENABLED - instance will self-terminate on panic/exit (this is a feature)
ExecStart=/usr/local/bin/synkti --project-name ${project_name} --region ${region}
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

# Enable and start service
echo "Enabling and starting Synkti service..."
systemctl daemon-reload
systemctl enable synkti.service
systemctl start synkti.service

echo ""
echo "=== Synkti Worker Setup Complete ==="
echo "Synkti orchestrator will manage vLLM container automatically"
echo ""
echo "Service status:"
systemctl status synkti.service --no-pager || true
echo ""
echo "View logs with: journalctl -u synkti -f"
echo ""
