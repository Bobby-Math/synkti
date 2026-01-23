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
      echo "  IAM: ${PROJECT_NAME}-worker (P2P architecture, no control plane)"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      exit 1
      ;;
  esac
done

echo "🚀 Synkti GitOps Bootstrap (P2P Architecture)"
echo "============================================"
echo "Project: $PROJECT_NAME"
echo "Region: $REGION"
echo ""

# Step 1: Build the orchestrator
echo "📦 Step 1: Building synkti..."
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(dirname "$SCRIPT_DIR")"
cd "$REPO_DIR/crates"
if ! cargo build --release -p synkti-orchestrator; then
  echo ""
  echo "❌ Build failed. Fix compilation errors first:"
  echo "   cd crates && cargo build -p synkti-orchestrator"
  exit 1
fi
BINARY_PATH="$REPO_DIR/crates/target/release/synkti"

if [ ! -f "$BINARY_PATH" ]; then
  echo "❌ Binary not found at $BINARY_PATH"
  exit 1
fi
echo "✅ Built: $BINARY_PATH"

# Step 2: Create infrastructure (get bucket names)
echo ""
echo "🏗️  Step 2: Creating infrastructure..."
cd "$REPO_DIR/infra"
if ! terraform init >/dev/null 2>&1; then
  echo "❌ Terraform init failed"
  exit 1
fi
if ! terraform apply -auto-approve -var="project_name=$PROJECT_NAME"; then
  echo ""
  echo "❌ Terraform apply failed. Check the errors above."
  echo "   Re-run with: cd infra && terraform plan -var='project_name=$PROJECT_NAME'"
  exit 1
fi

BUCKET_NAME=$(terraform output -raw models_bucket_name 2>/dev/null) || {
  echo "❌ Failed to get bucket name from terraform output"
  exit 1
}
CHECKPOINT_BUCKET=$(terraform output -raw checkpoint_bucket_name 2>/dev/null) || {
  echo "❌ Failed to get checkpoint bucket name from terraform output"
  exit 1
}

echo ""
echo "📋 Buckets:"
echo "  Models:      $BUCKET_NAME"
echo "  Checkpoints: $CHECKPOINT_BUCKET"

# Create owner marker so synkti knows infra exists
OWNER_MARKER="/tmp/synkti-${PROJECT_NAME}.owner"
echo $$ > "$OWNER_MARKER"
echo "✅ Created owner marker: $OWNER_MARKER"

# Step 3: Upload orchestrator binary to S3
echo ""
echo "📤 Step 3: Uploading orchestrator binary to S3..."
if ! aws s3 cp "$BINARY_PATH" "s3://${BUCKET_NAME}/bin/synkti" --region "$REGION"; then
  echo "❌ Binary upload failed"
  echo "   Manual upload: aws s3 cp ${BINARY_PATH} s3://${BUCKET_NAME}/bin/synkti --region ${REGION}"
  exit 1
fi
echo "✅ Binary uploaded to: s3://${BUCKET_NAME}/bin/synkti"

# Step 4: Check and upload model weights
echo ""
echo "📦 Step 4: Checking model weights..."

# Check if bucket has any models
MODEL_COUNT=$(aws s3 ls "s3://${BUCKET_NAME}/" --recursive --region "$REGION" 2>/dev/null | wc -l)

if [ -n "$MODEL_PATH" ] && [ -d "$MODEL_PATH" ]; then
  echo "📤 Uploading model weights from: $MODEL_PATH"
  MODEL_NAME=$(basename "$MODEL_PATH")
  aws s3 sync "$MODEL_PATH" "s3://${BUCKET_NAME}/${MODEL_NAME}/" \
    --region "$REGION"
  echo "✅ Model uploaded to: s3://${BUCKET_NAME}/${MODEL_NAME}/"
  MODEL_COUNT=$(aws s3 ls "s3://${BUCKET_NAME}/" --recursive --region "$REGION" 2>/dev/null | wc -l)
fi

# Step 5: Verification
echo ""
echo "🔍 Step 5: Verifying deployment..."
BOOTSTRAP_SUCCESS=true

# Check binary uploaded
BINARY_S3_PATH="s3://${BUCKET_NAME}/bin/synkti"
if aws s3 ls "$BINARY_S3_PATH" --region "$REGION" >/dev/null 2>&1; then
  # Get local binary size
  LOCAL_SIZE=$(stat -f%z "$BINARY_PATH" 2>/dev/null || stat -c%s "$BINARY_PATH" 2>/dev/null)
  # Get S3 binary size
  S3_SIZE=$(aws s3 ls "$BINARY_S3_PATH" --region "$REGION" --human-readable | awk '{print $3}')
  echo "✅ Binary uploaded: $BINARY_S3_PATH (local: ${LOCAL_SIZE} bytes)"
else
  echo "❌ Binary NOT found in S3: $BINARY_S3_PATH"
  BOOTSTRAP_SUCCESS=false
fi

# Check model weights exist
echo ""
if [ "$MODEL_COUNT" -eq 0 ]; then
  echo "❌ No model weights found in s3://${BUCKET_NAME}/"
  echo ""
  echo "   Upload models before launching workers:"
  echo "   aws s3 sync /path/to/model s3://${BUCKET_NAME}/model-name/"
  BOOTSTRAP_SUCCESS=false
else
  echo "✅ Model weights found: $MODEL_COUNT object(s) in s3://${BUCKET_NAME}/"
  echo "   Sample files:"
  aws s3 ls "s3://${BUCKET_NAME}/" --recursive --region "$REGION" | head -3 | sed 's/^/     /'
fi

# Step 6: Result and next steps
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [ "$BOOTSTRAP_SUCCESS" = true ]; then
  echo "✅ BOOTSTRAP SUCCESSFUL!"
  echo ""
  echo "📦 Project: $PROJECT_NAME"
  echo "🪣 Models:  s3://${BUCKET_NAME}/"
  echo "📝 Binary:  $BINARY_S3_PATH"
  echo ""
  echo "🚀 Ready to launch workers:"
  echo ""
  echo "   Via Terraform:"
  echo "   cd infra && terraform apply -var='project_name=${PROJECT_NAME}' -var='worker_count=2'"
  echo ""
  echo "   Via CLI:"
  echo "   ./target/release/synkti --project-name ${PROJECT_NAME}"
else
  echo "⚠️  BOOTSTRAP INCOMPLETE"
  echo ""
  echo "🔧 Troubleshooting:"
  echo ""
  echo "   If binary upload failed:"
  echo "   aws s3 cp ${BINARY_PATH} s3://${BUCKET_NAME}/bin/synkti --region ${REGION}"
  echo ""
  echo "   If models are missing:"
  echo "   aws s3 sync /path/to/model s3://${BUCKET_NAME}/model-name/ --region ${REGION}"
  echo ""
  echo "   If terraform failed:"
  echo "   cd infra && terraform plan -var='project_name=${PROJECT_NAME}'"
  echo ""
  echo "   Re-run bootstrap after fixing:"
  echo "   ./scripts/bootstrap.sh --project ${PROJECT_NAME}"
fi
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
