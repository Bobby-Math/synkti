# Synkti GitOps Guide

## The Bootstrap Problem

Bootstrap ONCE from the local machine, then GitOps takes over forever.

### Initial Setup

1. Set up IAM permissions for the user in the AWS Console.
2. Run `terraform apply` from your local machine to create the infrastructure:
   - S3 buckets (permanent resources, manual cleanup required)
   - EC2 instance for deploying the orchestrator
3. Deploy the orchestrator using one of:
   - **Manual**: Log into the instance via AWS Systems Manager Session Manager and deploy
   - **GitOps**: Push to trigger automated deployment

After bootstrap, all deployments are managed through GitOps.

---

## First Time Setup (Bootstrap)

### Step 1: Run Bootstrap Script

```bash
# Clone the repo
git clone https://github.com/your-org/synkti.git
cd synkti

# Run bootstrap (creates infra, uploads binary)
chmod +x scripts/bootstrap.sh
./scripts/bootstrap.sh --project my-prod
```

This does:
1. Builds `synkti-orchestrator` binary
2. Runs `terraform apply` to create infrastructure
3. Uploads binary to permanent S3 bucket
4. (Optional) Uploads model weights to S3

### Step 2: Update user_data Binary Location

Edit `infra/main.tf` and uncomment/update the binary download:

```hcl
user_data = <<-EOF
  ...
  # Download orchestrator from S3
  aws s3 cp s3://my-prod-models/bin/synkti-orchestrator /usr/local/bin/synkti-orchestrator
  chmod +x /usr/local/bin/synkti-orchestrator
  ...
EOF
```

### Step 3: Push to Trigger Deploy

```bash
git add infra/
git commit -m "Configure production deployment"
git push
```

GitHub Actions automatically runs `terraform apply`.

### Step 4: Verify

```bash
# Check orchestrator is running
aws ssm start-session \
  --target $(terraform output -raw control_plane_instance_ids | head -1) \
  --region us-east-1

# Inside instance:
systemctl status synkti
journalctl -u synkti -f
```

---

## Every Subsequent Time (GitOps)

After the initial bootstrap, **never manually SSH** again.

### Make a Change

```bash
# Edit infra, model config, etc.
vim infra/variables.tf

# Commit and push
git add .
git commit -m "Upgrade control plane to t3.large"
git push
```

### GitHub Actions Handles the Rest

```
Push → GitHub Actions → terraform apply → Rolling update
```

Zero manual intervention.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                          GitHub                                 │
│  ┌─────────────┐     ┌──────────────┐     ┌──────────────┐   │
│  │     Code    │────→│ GitHub Act   │────→│    Terraform │   │
│  │  (main.rs)  │     │    (.yml)    │     │     apply    │   │
│  └─────────────┘     └──────────────┘     └──────────────┘   │
│                             │                      │          │
│                             ▼                      ▼          │
│  ┌─────────────┐     ┌──────────────┐     ┌──────────────┐   │
│  │  Security   │     │     Plan     │     │   Comments   │   │
│  │   Scan      │     │   (PR only)  │     │   (PR only)  │   │
│  └─────────────┘     └──────────────┘     └──────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                           AWS                                   │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Control Plane Instance (t3.medium)                     │  │
│  │  ┌────────────────────────────────────────────────────┐ │  │
│  │  │  systemd → synkti-orchestrator → vLLM container  │ │  │
│  │  └────────────────────────────────────────────────────┘ │  │
│  │            ↓ Auto-starts on boot (user_data)           │  │
│  │  ┌────────────────────────────────────────────────────┐ │  │
│  │  │  Downloads:                                         │ │  │
│  │  │  - Binary from s3://{project}-models/bin/          │ │  │
│  │  │  - Model from s3://{project}-models/llama-2-7b/    │ │  │
│  │  └────────────────────────────────────────────────────┘ │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐    │
│  │ GPU Workers  │  │ S3: Checkpts │  │ S3: Models       │    │
│  │ (via CLI)    │  │  (ephemeral) │  │   (PERMANENT)    │    │
│  └──────────────┘  └──────────────┘  └──────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

---

## Required GitHub Secrets

Navigate to: **Repository → Settings → Secrets and variables → Actions**

| Secret | Value | Description |
|--------|-------|-------------|
| `AWS_ROLE_ARN` | `arn:aws:iam::ACCOUNT_ID:role/GitHubActions` | OIDC role for AWS access |

### Setting up OIDC (Recommended over long-lived keys)

1. **Create IAM Role in AWS:**
   ```hcl
   resource "aws_iam_role" "github_actions" {
     name = "GitHubActions"
     assume_role_policy = jsonencode({
       Version = "2012-10-17"
       Statement = [{
         Effect = "Allow"
         Principal = {
           Federated = "arn:aws:iam::ACCOUNT_ID:oidc-provider/token.actions.githubusercontent.com"
         }
         Action = "sts:AssumeRoleWithWebIdentity"
         Condition = {
           StringEquals = {
             "token.actions.githubusercontent.com:aud" = "sts.amazonaws.com"
           }
           StringLike = {
             "token.actions.githubusercontent.com:sub" = "repo:your-org/synkti:ref:refs/heads/main"
           }
         }
       }]
     })
   }

   resource "aws_iam_role_policy_attachment" "github_actions_admin" {
     role       = aws_iam_role.github_actions.name
     policy_arn = "arn:aws:iam::aws:policy/AdministratorAccess"
   }
   ```

2. **Add secret to GitHub:**
   - Name: `AWS_ROLE_ARN`
   - Value: `arn:aws:iam::YOUR_ACCOUNT_ID:role/GitHubActions`

---

## File Structure

```
synkti/
├── .github/
│   └── workflows/
│       └── deploy.yml        # GitOps automation
├── infra/
│   ├── main.tf                # Infrastructure (with user_data)
│   ├── variables.tf           # Configuration
│   ├── outputs.tf             # Output values
│   └── versions.tf            # Provider versions
├── scripts/
│   └── bootstrap.sh          # One-time setup
└── crates/
    └── applications/
        └── synkti-orchestrator/
            └── src/
                └── main.rs    # Orchestrator binary
```

---

## Troubleshooting

### Instance not starting orchestrator?

```bash
# Connect via SSM
aws ssm start-session --target i-xxxxxxxx --region us-east-1

# Check logs
journalctl -u synkti -n 100

# Check service status
systemctl status synkti

# Restart service
systemctl restart synkti
```

### GitHub Actions failing?

1. Check AWS role trust relationship
2. Verify GitHub repo settings → Actions → General → Workflow permissions
3. Check secrets are set correctly

### Model not downloading?

1. Verify models bucket name: `terraform output models_bucket_name`
2. Check IAM permissions include S3 read access
3. Verify S3 path in `--model-s3` flag
