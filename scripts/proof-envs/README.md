# Proof System Environment Configs

Each `.env` file in this directory configures a proof system deployment
environment. The `just proof-*` targets load the matching file based on the
`env` parameter (default: `alphanet`).

## Adding a new environment

1. Copy an existing file (e.g. `alphanet.env`) to `<your-env>.env`.
2. Fill in the values for your environment.
3. Run targets with `just proof-setup <your-env>`.

## What goes in the config file

Non-secret, environment-specific values:

| Variable | Description |
|----------|-------------|
| `KUBECONTEXT` | Kubernetes context for `kubectl` commands |
| `PROOF_NAMESPACE` | Namespace for the proof nitro worker pods |
| `PROOF_NITRO_IMAGE` | Container image for the nitro attestation worker |

## Required shell environment variables

These contain secrets and **must** be set in your shell before running
targets — they are intentionally **not** stored in config files:

| Variable | Used by |
|----------|---------|
| `PRIVATE_KEY` | `proof-deploy-nitro`, `proof-deploy-system`, `proof-certmanager-prewarm` |
| `OWNER` | `proof-deploy-nitro` |
| `OWNER_KEY` | `proof-approve-pcrs` |
| `L1_RPC_URL` | `proof-deploy-nitro`, `proof-deploy-system`, `proof-certmanager-prewarm`, `proof-approve-pcrs` |
| `WORLD_CHAIN_L2_CHAIN_ID` | `proof-deploy-system` |
| `ROLLUP_CONFIG_HASH` | `proof-deploy-system` |
| `CERT_MANAGER_ADDRESS` | `proof-certmanager-prewarm` |
| `NITRO_ATTESTATION_VERIFIER` | `proof-approve-pcrs` |
| `PCR0`, `PCR1`, `PCR2` | `proof-approve-pcrs` |
