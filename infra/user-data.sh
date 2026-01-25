#!/bin/bash
# Synkti Worker User Data (P2P Architecture)
# All nodes are workers that self-govern
# Template variables: ${project_name}, ${models_bucket}, ${region}, ${synkti_binary_s3_path}, ${model_s3_path}

set -eux

# Configuration from Terraform
PROJECT_NAME="${project_name}"
MODELS_BUCKET="${models_bucket}"
REGION="${region}"
SYNKTI_BINARY_S3="${synkti_binary_s3_path}"
MODEL_S3_PATH="${model_s3_path}"

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

# Create local model directory (use /opt instead of /tmp for more space)
MODEL_LOCAL_PATH="/opt/models"
mkdir -p "$MODEL_LOCAL_PATH"

# Download model weights from S3 (REQUIRED - no external dependencies)
echo "Downloading model weights from S3..."
echo "  Source: $MODEL_S3_PATH"
echo "  Target: $MODEL_LOCAL_PATH"
if ! aws s3 sync "$MODEL_S3_PATH" "$MODEL_LOCAL_PATH" --region "$REGION"; then
  echo "❌ FATAL: Failed to download model from S3"
  echo "   Source: $MODEL_S3_PATH"
  echo "   This is a REQUIRED dependency - no fallback to HuggingFace"
  echo "   Check:"
  echo "     1. S3 bucket exists and is accessible"
  echo "     2. Model files are present at $MODEL_S3_PATH"
  echo "     3. IAM role has s3:GetObject permission"
  echo "   Terminating instance - RAII will return borrowed resources"
  exit 1
fi
echo "✅ Model weights downloaded from S3"
MODEL_LOCAL_ENV="$MODEL_LOCAL_PATH"

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
Environment="MODEL_LOCAL_PATH=${MODEL_LOCAL_ENV}"
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
