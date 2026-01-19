#!/bin/bash
# Synkti GitOps Bootstrap Script
#
# This script prepares your Synkti deployment for GitOps.
# Run this ONCE to upload the orchestrator binary and model weights to S3.
#
# Usage:
#   ./scripts/bootstrap.sh --project my-prod

set -euo pipefail

# Default values
PROJECT_NAME="synkti-prod"
MODEL_PATH=""
REGION="us-east-1"

# Parse arguments
while [[ $# -gt 0 ]]; do
  case $1 in
    --project)
      PROJECT_NAME="$2"
      shift 2
      ;;
    --model-path)
      MODEL_PATH="$2"
      shift 2
      ;;
    --region)
      REGION="$2"
      shift 2
      ;;
    --help)
      echo "Usage: $0 [--project NAME] [--model-path PATH] [--region REGION]"
      echo ""
      echo "Options:"
      echo "  --project NAME    Project name (default: synkti-prod)"
      echo "  --model-path PATH Path to local model directory to upload"
      echo "  --region REGION   AWS region (default: us-east-1)"
      echo ""
      echo "All resources are named based on project_name:"
      echo "  S3: ${PROJECT_NAME}-models, ${PROJECT_NAME}-checkpoints-xxx"
      echo "  IAM: ${PROJECT_NAME}-control-plane, ${PROJECT_NAME}-worker"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      exit 1
      ;;
  esac
done

echo "🚀 Synkti GitOps Bootstrap"
echo "=========================="
echo "Project: $PROJECT_NAME"
echo "Region: $REGION"
echo ""

# Step 1: Build the orchestrator
echo "📦 Step 1: Building synkti-orchestrator..."
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(dirname "$SCRIPT_DIR")"
cd "$REPO_DIR/crates"
cargo build --release -p synkti-orchestrator
BINARY_PATH="$REPO_DIR/crates/target/release/synkti-orchestrator"

if [ ! -f "$BINARY_PATH" ]; then
  echo "❌ Binary not found at $BINARY_PATH"
  exit 1
fi
echo "✅ Built: $BINARY_PATH"

# Step 2: Create infrastructure (get bucket names)
echo ""
echo "🏗️  Step 2: Creating infrastructure..."
cd "$REPO_DIR/infra"
terraform init
terraform apply -auto-approve -var="project_name=$PROJECT_NAME"

BUCKET_NAME=$(terraform output -raw models_bucket_name)
CHECKPOINT_BUCKET=$(terraform output -raw checkpoint_bucket_name)

echo ""
echo "📋 Buckets:"
echo "  Models:      $BUCKET_NAME"
echo "  Checkpoints: $CHECKPOINT_BUCKET"

# Step 3: Upload orchestrator binary to S3
echo ""
echo "📤 Step 3: Uploading orchestrator binary to S3..."
aws s3 cp "$BINARY_PATH" "s3://${BUCKET_NAME}/bin/synkti-orchestrator" \
  --region "$REGION"
echo "✅ Binary uploaded to: s3://${BUCKET_NAME}/bin/synkti-orchestrator"

# Step 4: Upload model weights if provided
if [ -n "$MODEL_PATH" ] && [ -d "$MODEL_PATH" ]; then
  echo ""
  echo "📤 Step 4: Uploading model weights to S3..."
  MODEL_NAME=$(basename "$MODEL_PATH")
  aws s3 sync "$MODEL_PATH" "s3://${BUCKET_NAME}/${MODEL_NAME}/" \
    --region "$REGION"
  echo "✅ Model uploaded to: s3://${BUCKET_NAME}/${MODEL_NAME}/"
else
  echo ""
  echo "⏭️  Step 4: Skipped (no --model-path provided)"
  echo "   Upload models later with:"
  echo "   aws s3 sync /path/to/model s3://${BUCKET_NAME}/model-name/"
fi

# Step 5: Next steps
echo ""
echo "📝 Step 5: Next steps:"
echo ""
echo "1. Commit and push changes:"
echo "   git add infra/"
echo "   git commit -m 'Configure GitOps deployment'"
echo "   git push"
echo ""
echo "2. GitHub Actions will automatically deploy!"
echo ""
echo "✅ Bootstrap complete!"
echo ""
echo "Your project: $PROJECT_NAME"
echo "Models bucket: s3://${BUCKET_NAME}/"
echo "Upload model weights there, then reference with --model-s3 flag."
