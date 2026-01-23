# Synkti GitOps Guide

## The Bootstrap Problem

Bootstrap ONCE from the local machine, then GitOps takes over forever.

### Initial Setup

1. Set up IAM permissions for the user in the AWS Console.
2. Run `terraform apply` from your local machine to create the infrastructure:
   - S3 buckets (permanent resources, manual cleanup required)
   - IAM roles for spot workers
3. Deploy spot workers with the orchestrator binary already in S3
4. Each worker auto-starts the orchestrator on boot via user_data

After bootstrap, infrastructure updates are managed through GitOps.

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
1. Builds `synkti` binary
2. Runs `terraform apply` to create infrastructure
3. Uploads binary to permanent S3 bucket: `s3://{project}-models/bin/synkti`
4. (Optional) Uploads model weights to S3

### Step 2: Launch Spot Workers

```bash
# Using the synkti CLI (auto-creates infra if needed)
./scripts/target/release/synkti --project-name my-prod

# Or launch via terraform manually
cd infra
terraform apply -var="project_name=my-prod" -var="worker_count=2"
```

### Step 3: Verify

```bash
# Check that nodes are tagged and discoverable
aws ec2 describe-instances \
  --filters "Name=tag:SynktiCluster,Values=my-prod" \
  --query "Reservations[].Instances[].InstanceId" \
  --output text

# Connect to any worker via SSM
INSTANCE_ID=$(aws ec2 describe-instances \
  --filters "Name=tag:SynktiCluster,Values=my-prod" \
  --query "Reservations[0].Instances[0].InstanceId" \
  --output text)

aws ssm start-session --target $INSTANCE_ID --region us-east-1

# Inside instance:
journalctl -u synkti -f
```

---

## Every Subsequent Time (GitOps)

After the initial bootstrap, **never manually SSH** for deployments.

### Make a Change

```bash
# Edit infra, model config, etc.
vim infra/variables.tf

# Commit and push
git add .
git commit -m "Add more spot workers"
git push
```

### GitHub Actions Handles the Rest

```
Push → GitHub Actions → terraform apply → New workers join cluster
```

Zero manual intervention.

---

## Architecture: P2P Choreography

```
┌─────────────────────────────────────────────────────────────────┐
│                          GitHub                                 │
│  ┌─────────────┐     ┌──────────────┐     ┌──────────────┐   │
│  │     Code    │────→│ GitHub Act   │────→│    Terraform │   │
│  │  (main.rs)  │     │    (.yml)    │     │     apply    │   │
│  └─────────────┘     └──────────────┘     └──────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                           AWS                                   │
│                                                                 │
│   ┌──────────────┐        ┌──────────────┐        ┌───────────┐│
│   │  Spot Node A │◄──────►│  Spot Node B │◄──────►│ Spot Node ││
│   │  g4dn.xlarge │  P2P   │  g4dn.xlarge │  P2P   │    C     ││
│   │              │        │              │        │           ││
│   │  ┌────────┐  │        │  ┌────────┐  │        │ ┌───────┐││
│   │  │synkti  │  │        │  │synkti  │  │        │ │synkti│││
│   │  │orchest.│  │        │  │orchest.│  │        │ │orch. │││
│   │  │  +     │  │        │  │  +     │  │        │ │  +   │││
│   │  │ vLLM   │  │        │  │ vLLM   │  │        │ │ vLLM │││
│   │  └────────┘  │        │  └────────┘  │        │ └───────┘││
│   │              │        │              │        │           ││
│   │  EC2 tags:   │        │  EC2 tags:   │        │ EC2 tags: ││
│   │  SynktiCluster│        │  SynktiCluster│       │SynktiCluster│
│   │  SynktiRole= │        │  SynktiRole= │        │SynktiRole=││
│   │  worker      │        │  worker      │        │worker    ││
│   └──────────────┘        └──────────────┘        └───────────┘│
│          │                        │                       │       │
│          └────────────────────────┴───────────────────────┘   │
│                             │                                   │
│                             ▼                                   │
│                  Peer Discovery (EC2 tags)                      │
│                  Each node self-governing                       │
│                  No central control plane                       │
│                                                                 │
│  ┌──────────────┐           ┌───────────────────┐              │
│  │ S3: Models   │           │ S3: Checkpoints   │              │
│  │  (PERMANENT) │           │  (ephemeral)      │              │
│  │              │           │                   │              │
│  │ - synkti bin │           │ - KV cache        │              │
│  │ - qwen2.5-7b │           │ - state snapshots  │              │
│  └──────────────┘           └───────────────────┘              │
└─────────────────────────────────────────────────────────────────┘
```

**Key difference from centralized orchestrators:**

| Traditional (K8s) | Synkti (P2P) |
|-------------------|---------------|
| Single control plane (API server, etcd) | No control plane |
| Central scheduler | Each node self-assigns |
| State drift (model ≠ reality) | Truth at the edge |
| SPOF at control plane | No SPOF |
| Reconciliation overhead | No reconciliation |

---

## How P2P Discovery Works

Each node on startup:

1. **Tags itself** with `SynktiCluster={project}` and `SynktiRole=worker`
2. **Discovers peers** by querying EC2 for instances with matching tags
3. **Refreshes peer list** every 30 seconds
4. **Untags itself** on graceful shutdown

```rust
// What each node does on startup
let my_id = get_instance_id().await?;
tag_self_as_worker(&ec2, &my_id, "my-prod").await?;

// Find peers
let peers = discover_peers("my-prod").await?;
// → Returns all instances with SynktiCluster=my-prod
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
│   ├── main.tf                # Infrastructure (IAM, S3, SGs)
│   ├── variables.tf           # Configuration
│   ├── outputs.tf             # Output values
│   └── user-data.sh           # Worker boot script
├── scripts/
│   └── bootstrap.sh          # One-time setup
└── crates/
   └── applications/
       └── synkti-orchestrator/
           └── src/
               ├── main.rs    # CLI entry point
               ├── discovery.rs  # P2P peer discovery
               ├── failover.rs   # Stateless failover
               ├── drain.rs      # Graceful request drain
               └── ...
```

---

## Troubleshooting

### Workers not discovering each other?

```bash
# Check EC2 tags
aws ec2 describe-tags \
  --filters "Name=resource-id,Values=i-xxxxxxxx" \
  --query "Tags[?Key==`SynktiCluster`]" \
  --region us-east-1

# Verify all workers have:
# - SynktiCluster = <project-name>
# - SynktiRole = worker
```

### Orchestrator not starting?

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
3. Verify S3 path in user-data.sh

---

## Stopping a Cluster

Since there's no central control plane, stop by terminating instances:

```bash
# Find all instances in cluster
aws ec2 describe-instances \
  --filters "Name=tag:SynktiCluster,Values=my-prod" \
  --query "Reservations[].Instances[].InstanceId" \
  --output text | xargs -I {} aws ec2 terminate-instances --instance-ids {}

# Or destroy infra (includes IAM, S3, etc.)
cd infra
terraform destroy -var="project_name=my-prod"

# Note: S3 models bucket is NOT destroyed (prevent_destroy=true)
```
