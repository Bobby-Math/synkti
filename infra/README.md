# Synkti Infrastructure (P2P Architecture)

Terraform configuration for deploying Synkti on AWS using a **peer-to-peer architecture** with no central control plane.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    P2P ARCHITECTURE                             │
│                                                                 │
│   ┌─────────┐      ┌─────────┐      ┌─────────┐               │
│   │ Worker  │◄────►│ Worker  │◄────►│ Worker  │               │
│   │  Node 1 │      │  Node 2 │      │  Node 3 │               │
│   └─────────┘      └─────────┘      └─────────┘               │
│       │                │                 │                     │
│       └────────────────┴─────────────────┘                     │
│                     │                                           │
│              EC2 Tags (Discovery)                               │
│              - SynktiCluster=<project>                          │
│              - SynktiRole=worker                                │
│                                                                 │
│  No control plane. Each node self-governs.                    │
└─────────────────────────────────────────────────────────────────┘
```

## What Gets Created

| Resource | Name Pattern | Permanence |
|----------|--------------|------------|
| S3 models bucket | `{project}-models` | **Permanent** |
| S3 checkpoints bucket | `{project}-checkpoints-*` | Ephemeral |
| IAM worker role | `{project}-worker` | Ephemeral |
| Security group | `{project}-worker` | Ephemeral |
| EC2 workers (optional) | `{project}-worker-N` | Ephemeral |

## Quick Start

```bash
terraform init
terraform plan -var="project_name=my-project"
terraform apply -var="project_name=my-project"
```

## Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `project_name` | `"synkti"` | Project namespace for all resources |
| `aws_region` | `"us-east-1"` | AWS region |
| `worker_count` | `0` | Number of workers to create via Terraform |
| `worker_instance_type` | `"g4dn.xlarge"` | GPU instance type |
| `allowed_cidr_blocks` | `["0.0.0.0/0"]` | CIDR allowed to access workers |
| `synkti_binary_s3_path` | `""` | S3 path to synkti binary |
| `model_s3_path` | `""` | S3 path to model weights |

## Deterministic Applies (Production)

For production, save the plan and apply exactly that plan:

```bash
terraform plan -var="project_name=my-project" -out=tfplan
terraform apply tfplan
```

**Why?** The `tfplan` file contains the exact planned changes. `terraform apply tfplan` executes **exactly** what was planned — no recalculation, no surprises.

## Bootstrap Workflow

The infrastructure creates empty buckets. You provide the content:

```bash
# 1. Create infrastructure
terraform apply -var="project_name=my-project"

# 2. Upload the synkti binary
./scripts/upload-binary.sh --project-name my-project

# 3. Upload model weights
./scripts/upload-model.sh --project-name my-project --model Qwen/Qwen2.5-7B-Instruct

# 4. Launch workers (either via terraform or synkti CLI)
terraform apply -var="project_name=my-project" -var="worker_count=3"
# OR: synkti --project-name my-project  # Deploys and monitors
```

## Security Groups

The worker security group allows:
- **SSH (22)** - from `allowed_cidr_blocks`
- **HTTP (80)** - from `allowed_cidr_blocks`
- **HTTPS (443)** - from `allowed_cidr_blocks`
- **vLLM API (8000)** - from `allowed_cidr_blocks`
- **P2P communication** - between workers (self-referential)

## Cleanup

```bash
terraform destroy -var="project_name=my-project"
```

**Note:** The models bucket is NOT destroyed (`prevent_destroy=true`) to protect your model weights. Clean up manually if needed:

```bash
aws s3 rb s3://my-project-models --force
```

## Outputs

After `terraform apply`, you'll see:

```bash
worker_instance_ids = ["i-xxx", "i-yyy"]
worker_instance_profile_name = "my-project-worker"
models_bucket_name = "my-project-models"
worker_sg_id = "sg-xxx"
```

Use these values with the `synkti` CLI or for manual operations.
