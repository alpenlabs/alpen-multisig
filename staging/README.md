# Staging — EC2 Deployment

Full local stack (Bitcoin regtest + Strata ASM + Orchestrator + regtest-dev-api) via Docker Compose.

## Services

| Service | Port | Description |
|---------|------|-------------|
| bitcoin | 18443 | Bitcoin Core regtest (RPC) |
| asm | 8080 | Strata ASM runner |
| orchestrator | 3000 | Orchestrator backend API |
| regtest-dev-api | 3001 | Mine blocks / faucet endpoints |

## Prerequisites

```bash
# Install Docker and Docker Compose plugin (Amazon Linux 2023)
sudo dnf install -y docker
sudo systemctl enable --now docker
sudo usermod -aG docker ec2-user   # re-login after this

sudo mkdir -p /usr/local/lib/docker/cli-plugins
sudo curl -SL https://github.com/docker/compose/releases/latest/download/docker-compose-linux-x86_64 \
  -o /usr/local/lib/docker/cli-plugins/docker-compose
sudo chmod +x /usr/local/lib/docker/cli-plugins/docker-compose
```

## First deploy

```bash
# Clone and move into the repo
git clone <repo-url> alpen-multisig
cd alpen-multisig

# Revoke and rotate the previously exposed database credential before deployment.
# Provision a dedicated role that is not the Neon project owner, with only the
# staging schema, migration, and application permissions it needs. Export its URL from the
# approved secret manager. For an interactive deployment, read it without saving
# it in shell history or a repository file:
export DATABASE_URL
read -rsp "Rotated staging DATABASE_URL: " DATABASE_URL
printf '\n'

# Build and start all services (first build takes ~10 min)
docker compose -f staging/docker-compose.yml up -d --build
unset DATABASE_URL

# Follow logs
docker compose -f staging/docker-compose.yml logs -f
```

The staging orchestrator refuses to start without `DATABASE_URL`; never store the value in the
repository (including `staging/.env`). Audit the database provider's access logs for use of the
exposed owner account, and restrict database network access to the EC2 host's IP (Neon IP allowlist).

If `up` runs without `DATABASE_URL` exported (for example via `sudo`, which drops the environment),
compose still exits 0 but the orchestrator restarts in a loop. Check `docker compose ps` or the logs.

On first start, `bitcoin` mines 101 regtest blocks and writes `asm-params.json`.
Subsequent restarts skip this step (flag file in the Docker volume).

## Update

```bash
git pull

# Export the rotated least-privilege URL from the approved secret manager first.
docker compose -f staging/docker-compose.yml up -d --build
unset DATABASE_URL
```

## Useful commands

```bash
# Status
docker compose -f staging/docker-compose.yml ps

# Stop everything
docker compose -f staging/docker-compose.yml down

# Wipe volumes (full reset — chain state is lost)
docker compose -f staging/docker-compose.yml down -v
```

## regtest-dev-api

Exposes simple dev helpers against the local Bitcoin node.

**Mine blocks**
```bash
# Mine 5 blocks
curl -X POST "http://<ec2-ip>:3001/mine?count=5"
```

**Faucet — send BTC to an address**
```bash
curl -X POST "http://<ec2-ip>:3001/faucet" \
  -H "Content-Type: application/json" \
  -d '{"address":"bcrt1q...","amount_btc":1.0}'
```

> The faucet also mines 1 block to confirm the transaction.

## Security note

All RPC credentials are hardcoded dev defaults (`user`/`password`).
Make sure ports 18443, 8080, 3000, and 3001 are restricted to trusted IPs
in your EC2 security group — do not expose them publicly.
