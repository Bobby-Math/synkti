# Synkti Infrastructure

Terraform configuration for deploying Synkti on AWS.

## Quick Start

```bash
terraform init
terraform plan -var="project_name=my-project"
terraform apply -var="project_name=my-project"
```

## Deterministic Applies (Production)

For production, save the plan and apply exactly that plan:

```bash
terraform plan -var="project_name=my-project" -out=tfplan
terraform apply tfplan
```

**Why?** The `tfplan` file contains the exact planned changes. `terraform apply tfplan` executes **exactly** what was planned — no recalculation, no surprises.

