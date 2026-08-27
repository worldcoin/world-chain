# ==============================================================================
# Proof System — vkeys and L1 contract deployment
# ==============================================================================
#
# These recipes need only an L1/L2 RPC, forge/cast, and this checkout. Nothing
# here talks to Kubernetes.
#
# The cluster-side phases — measuring the running Nitro enclave, fetching its
# attestation, and registering its signing key — are not in this repo. They
# require an authenticated kubecontext and vsock access from inside the worker
# pod, so they live with the deployment topology in the infrastructure repo:
#
#   proof-pcrs, proof-attestation, proof-chain-id,
#   proof-verify-pcrs, proof-register-key, proof-setup
#
# `just proof-setup <env>` there orchestrates both sides: it reads the cluster,
# then delegates each phase below via $WORLD_CHAIN_REPO.
#
# Workflow phases owned by this repo:
#   Phase 0a  proof-rollup-config-hash   – Compute rollup config hash from an L2 RPC
#   Phase 1   proof-deploy-nitro         – Deploy Nitro attestation contracts
#   Phase 1b  proof-deploy-mocks         – Deploy proof-lane test doubles (devnet only)
#   Phase 2   proof-deploy-system        – Deploy proof system implementation and vault
#   Phase 2b  proof-fund-mock-bonds      – Fund persistent devnet bond accounts (devnet only)
#   Phase 3a  proof-certmanager-prewarm  – Pre-warm CertManager with CA certs
#   Phase 3b  proof-approve-pcrs         – Approve a PCR set on the verifier
#   —         proof-activate-system      – Validate, register, and activate WIP-1006
#
# Required env vars (varies by target):
#   PRIVATE_KEY, OWNER, OWNER_KEY, L1_RPC_URL,
#   WORLD_CHAIN_L2_CHAIN_ID, ROLLUP_CONFIG_HASH,
#   AGGREGATION_VKEY, RANGE_VKEY_COMMITMENT, TEE_IMAGE_ID,
#   CERT_MANAGER_ADDRESS, NITRO_ATTESTATION_VERIFIER,
#   DISPUTE_GAME_FACTORY, ANCHOR_STATE_REGISTRY, SYSTEM_CONFIG,
#   OP_CHAIN_PROXY_ADMIN, WLD_TOKEN, DGF_OWNER_KEY,
#   GUARDIAN_KEY (when switching the respected game type)
#
# PCR0/PCR1/PCR2 are inputs here, not discovered: measure the running enclave
# with `just proof-pcrs <env>` in the infrastructure repo.
#
# Simulation mode (no on-chain broadcast):
#   dry_run=true   Simulate without broadcasting (all other steps still run)
# ==============================================================================

# Set dry_run=true on the command line to simulate without broadcasting.
dry_run := "false"

# Prove a PBH transaction
[group('proof')]
prove *args='':
    cargo run -p xtask -- prove $@

# Compute the on-chain verification keys for the SP1 proof ELFs.
# The ELFs are compiled and embedded at build time by
# `proofs/backends/sp1/elfs/build.rs` (sp1_build::build_program_with_args
# with docker:true at the pinned SP1 toolchain tag), so just running
# `cargo run` is enough — no separate ELF build step is required.
[doc("Print the on-chain verification keys for the SP1 proof ELFs.")]
[group('proof')]
proof-vkeys *args='':
    cargo run --release -p world-chain-prover-sp1 -- vkeys $@

# Recompute vkeys from the embedded ELFs and update the `.sp1` half of
# proofs/measurements.json. The `.nitro` half is left alone: it is measured by
# scripts/build-eif.sh on Linux x86_64 and nothing here can recompute it.
# Requires Docker and the SP1 toolchain (sp1up v6.3.1) for reproducible ELF builds.
[doc("Recompute vkeys from the embedded ELFs and update measurements.json.")]
[group('proof')]
update-proof-vkeys:
    cargo run -p world-chain-prover-sp1 -- vkeys --output /tmp/vkeys-update.json
    jq -S --slurpfile sp1 /tmp/vkeys-update.json '.sp1 = $sp1[0]' proofs/measurements.json > /tmp/measurements-update.json
    mv /tmp/measurements-update.json proofs/measurements.json

# Recomputes both halves: `.sp1` via the recipe above, then `.nitro` via
# scripts/build-eif.sh, whose output already carries the fresh `.sp1` forward.
# Needs Linux x86_64 with Nix (EIF), plus Docker and sp1up (vkeys).
[doc("Recompute all of proofs/measurements.json: sp1 vkeys and nitro PCRs.")]
[group('proof')]
update-measurements: update-proof-vkeys
    scripts/build-eif.sh target/eif
    cp target/eif/measurements.json proofs/measurements.json

# Verify that the `.sp1` half of proofs/measurements.json matches what the current
# source produces. Uses jq -S to normalize key ordering before comparing, so the diff
# is not sensitive to JSON insertion order. Used by CI. Fails if they differ.
[doc("Assert the sp1 half of measurements.json matches what the source produces. Used by CI.")]
[group('proof')]
verify-proof-vkeys:
    cargo run -p world-chain-prover-sp1 -- vkeys --output /tmp/vkeys-actual.json
    jq -S .sp1 proofs/measurements.json > /tmp/vkeys-committed.json
    jq -S . /tmp/vkeys-actual.json > /tmp/vkeys-actual-normalized.json
    diff /tmp/vkeys-committed.json /tmp/vkeys-actual-normalized.json || (echo "ERROR: the sp1 half of proofs/measurements.json is out of date. Run 'just update-proof-vkeys' to regenerate." && exit 1)

# Everything in the measured workspace's lockfile is compiled into the EIF, so a
# compromised crate there ships inside the attested TCB. cargo-deny rejects known-bad
# crates (advisories, unknown sources, duplicate versions); cargo-vet fails when the
# lockfile gains a crate version with neither an imported audit nor an exemption.
[doc("Audit the measured enclave dependency tree with cargo-deny and cargo-vet. Used by CI.")]
[group('proof')]
audit-enclave-deps:
    cd proofs/measured/nitro-enclave && cargo deny check advisories bans sources
    cd proofs/measured/nitro-enclave && cargo vet --locked

# Phase 0a – Compute and print the rollup config hash.
# Sources (checked in priority order):
#   L2_RPC_URL        – op-node RPC endpoint (port 9545, NOT the execution client on 8545)
#   ROLLUP_CONFIG_URL – URL to download the rollup config JSON from
#   ROLLUP_CONFIG     – local file path to an existing rollup config JSON
# For a deployed environment, the infrastructure repo's `proof-rollup-config-hash`
# recipe port-forwards to the op-node and calls this one with L2_RPC_URL set.
[doc("Phase 0a - Compute the rollup config hash from an L2 RPC or config file.")]
[group('proof')]
proof-rollup-config-hash env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${L2_RPC_URL:-}" ]; then
        echo "Fetching rollup config from op-node at ${L2_RPC_URL}…" >&2
        cargo run -p world-chain-prover-nitro -- hash-rollup-config --l2-rpc "$L2_RPC_URL"
    elif [ -n "${ROLLUP_CONFIG_URL:-}" ]; then
        echo "Downloading rollup config from ${ROLLUP_CONFIG_URL}…" >&2
        curl -sfSL "$ROLLUP_CONFIG_URL" -o /tmp/rollup.json
        cargo run -p world-chain-prover-nitro -- hash-rollup-config --rollup-config /tmp/rollup.json
    elif [ -n "${ROLLUP_CONFIG:-}" ]; then
        echo "Using local rollup config: $ROLLUP_CONFIG" >&2
        cargo run -p world-chain-prover-nitro -- hash-rollup-config --rollup-config "$ROLLUP_CONFIG"
    else
        echo "Error: set one of L2_RPC_URL, ROLLUP_CONFIG_URL or ROLLUP_CONFIG." >&2
        echo "  For a deployed env, use the infrastructure repo's 'proof-rollup-config-hash {{env}}'," >&2
        echo "  which port-forwards to the op-node and sets L2_RPC_URL for you." >&2
        exit 1
    fi

# Phase 1 – Deploy the Nitro attestation stack.
[group('proof')]
proof-deploy-nitro env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${OWNER:?OWNER is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    BROADCAST_FLAG=""
    if [ "{{dry_run}}" = "false" ]; then
        BROADCAST_FLAG="--broadcast"
    fi
    # A dry run must not overwrite the record of the live Nitro stack: the simulated addresses
    # are never deployed, and this file is what proof-approve-pcrs and proof-register-key read.
    if [ -n "$BROADCAST_FLAG" ]; then
        export NITRO_DEPLOYMENT_OUT="deployments/{{env}}-nitro.json"
    else
        export NITRO_DEPLOYMENT_OUT="deployments/{{env}}-nitro.dryrun.json"
    fi
    echo "Deploying Nitro contracts (deployment → $NITRO_DEPLOYMENT_OUT)$([ -n "$BROADCAST_FLAG" ] || echo ' [DRY RUN]')…"
    cd pkg/contracts && mkdir -p deployments && forge script scripts/devnet/DeployNitro.s.sol:DeployNitro \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" $BROADCAST_FLAG --slow

# Writes deployments/<env>-proof-mocks.json for proof-deploy-system to consume.
# MockRootIdVerifier accepts every proof: never run this against a chain whose
# withdrawals matter.
# Phase 1b (devnet only) – Deploy the proof-lane test doubles (MOCKS, accept any proof).
[doc("Phase 1b (devnet only) - Deploy proof-lane test doubles. MOCKS: accept any proof.")]
[group('proof')]
proof-deploy-mocks env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    BROADCAST_FLAG=""
    if [ "{{dry_run}}" = "false" ]; then
        BROADCAST_FLAG="--broadcast"
    fi
    # Same reasoning as proof-deploy-system: a dry run must not clobber a real record.
    if [ -n "$BROADCAST_FLAG" ]; then
        export PROOF_MOCKS_DEPLOYMENT_OUT="deployments/{{env}}-proof-mocks.json"
    else
        export PROOF_MOCKS_DEPLOYMENT_OUT="deployments/{{env}}-proof-mocks.dryrun.json"
    fi
    echo "WARNING: deploying MOCK proof-system dependencies (accept-any verifiers and mintable WLD) for '{{env}}'." >&2
    echo "Deploying proof mocks (deployment → $PROOF_MOCKS_DEPLOYMENT_OUT)$([ -n "$BROADCAST_FLAG" ] || echo ' [DRY RUN]')…"
    cd pkg/contracts && mkdir -p deployments && forge script scripts/devnet/DeployProofMocks.s.sol:DeployProofMocks \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" $BROADCAST_FLAG --slow

# The three proof-lane verifiers are required inputs — this
# script never deploys them, and rejects addresses that hold no code or that repeat
# across lanes. Reuse the SP1 verifier when only the game-pinned vkeys change; rotate
# its address only if the verifier gateway or proof interface changes. For a devnet,
# run `proof-deploy-mocks` first and read the four addresses out of
# deployments/<env>-proof-mocks.json. An existing deployment record supplies the singleton
# WLD vault automatically during rotations unless WLD_STAKING_VAULT is set explicitly.
# Phase 2 – Deploy the proof system implementation and vault.
[group('proof')]
proof-deploy-system env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${WORLD_CHAIN_L2_CHAIN_ID:?WORLD_CHAIN_L2_CHAIN_ID is required}"
    : "${ROLLUP_CONFIG_HASH:?ROLLUP_CONFIG_HASH is required}"
    : "${AGGREGATION_VKEY:?AGGREGATION_VKEY is required}"
    : "${RANGE_VKEY_COMMITMENT:?RANGE_VKEY_COMMITMENT is required}"
    : "${TEE_IMAGE_ID:?TEE_IMAGE_ID is required}"
    : "${DISPUTE_GAME_FACTORY:?DISPUTE_GAME_FACTORY is required (op-deployer DisputeGameFactoryProxy)}"
    : "${ANCHOR_STATE_REGISTRY:?ANCHOR_STATE_REGISTRY is required (op-deployer AnchorStateRegistryProxy)}"
    : "${SYSTEM_CONFIG:?SYSTEM_CONFIG is required (op-deployer SystemConfigProxy)}"
    : "${OP_CHAIN_PROXY_ADMIN:?OP_CHAIN_PROXY_ADMIN is required (op-deployer ProxyAdmin)}"
    : "${WLD_TOKEN:?WLD_TOKEN is required}"
    : "${PROTOCOL_FEE_RECIPIENT:?PROTOCOL_FEE_RECIPIENT is required (proof-timeout forfeitures)}"
    : "${VALIDITY_PROOF_VERIFIER:?VALIDITY_PROOF_VERIFIER is required (e.g. SP1ValidityVerifier; devnet: proof-deploy-mocks)}"
    : "${TEE_VERIFIER:?TEE_VERIFIER is required (e.g. NitroProofVerifier from proof-deploy-nitro)}"
    : "${SECURITY_COUNCIL_VERIFIER:?SECURITY_COUNCIL_VERIFIER is required (council attestation verifier)}"
    export CHALLENGE_PERIOD="${CHALLENGE_PERIOD:-86400}"
    export PROOF_PERIOD="${PROOF_PERIOD:-604800}"
    export PROPOSER_BOND="${PROPOSER_BOND:-10000000000000000}"
    export CHALLENGER_BOND="${CHALLENGER_BOND:-10000000000000000}"
    export PROOF_THRESHOLD="${PROOF_THRESHOLD:-2}"
    export WLD_WITHDRAWAL_DELAY="${WLD_WITHDRAWAL_DELAY:-300}"
    DEPLOYMENTS_FILE="pkg/contracts/deployments/{{env}}-proof-system.json"
    if [ -z "${WLD_STAKING_VAULT:-}" ] && [ -f "$DEPLOYMENTS_FILE" ]; then
        WLD_STAKING_VAULT=$(jq -r '.wldStakingVault // empty' "$DEPLOYMENTS_FILE")
        export WLD_STAKING_VAULT
    fi
    if [ -z "${WLD_STAKING_VAULT:-}" ]; then
        : "${OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY:?OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY is required for the first vault deployment}"
    fi
    BROADCAST_FLAG=""
    if [ "{{dry_run}}" = "false" ]; then
        BROADCAST_FLAG="--broadcast"
    fi
    echo "Proof-system parameters:" >&2
    echo "  challenge period: $CHALLENGE_PERIOD seconds" >&2
    echo "  proof period: $PROOF_PERIOD seconds" >&2
    echo "  proposer bond: $PROPOSER_BOND WLD base units" >&2
    echo "  challenger bond: $CHALLENGER_BOND WLD base units" >&2
    echo "  WLD vault: ${WLD_STAKING_VAULT:-new singleton}" >&2
    echo "  aggregation vkey: $AGGREGATION_VKEY" >&2
    echo "  range vkey commitment: $RANGE_VKEY_COMMITMENT" >&2
    echo "  TEE image ID: $TEE_IMAGE_ID" >&2
    # A dry run must never overwrite the record of a live deployment: the simulated game and
    # contract addresses are never deployed, so writing them to the real path silently replaces a
    # true record with fictional addresses.
    if [ -n "$BROADCAST_FLAG" ]; then
        export PROOF_SYSTEM_DEPLOYMENT_OUT="deployments/{{env}}-proof-system.json"
    else
        export PROOF_SYSTEM_DEPLOYMENT_OUT="deployments/{{env}}-proof-system.dryrun.json"
    fi
    echo "Deploying proof system contracts (deployment → $PROOF_SYSTEM_DEPLOYMENT_OUT)$([ -n "$BROADCAST_FLAG" ] || echo ' [DRY RUN]')…"
    cd pkg/contracts && mkdir -p deployments && forge script scripts/devnet/DeployProofSystem.s.sol:DeployProofSystem \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" $BROADCAST_FLAG --slow

# Phase 2b (devnet only) – Mint mock WLD and top up the proposer and challenger vault balances.
# Uses depositFor so the funding operator never needs either service's signing key. Targets make
# the operation idempotent and account for WLD already returned by settled games.
[doc("Phase 2b (devnet only) - Fund proposer and challenger mock-WLD vault balances.")]
[group('proof')]
proof-fund-mock-bonds env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${PROPOSER_ADDRESS:?PROPOSER_ADDRESS is required}"
    : "${CHALLENGER_ADDRESS:?CHALLENGER_ADDRESS is required}"
    MOCKS_FILE="pkg/contracts/deployments/{{env}}-proof-mocks.json"
    SYSTEM_FILE="pkg/contracts/deployments/{{env}}-proof-system.json"
    if [ -z "${WLD_TOKEN:-}" ] && [ -f "$MOCKS_FILE" ]; then
        WLD_TOKEN=$(jq -r '.wldToken // empty' "$MOCKS_FILE")
        export WLD_TOKEN
    fi
    if [ -z "${WLD_STAKING_VAULT:-}" ] && [ -f "$SYSTEM_FILE" ]; then
        WLD_STAKING_VAULT=$(jq -r '.wldStakingVault // empty' "$SYSTEM_FILE")
        export WLD_STAKING_VAULT
    fi
    : "${WLD_TOKEN:?WLD_TOKEN is required (or deploy proof mocks first)}"
    : "${WLD_STAKING_VAULT:?WLD_STAKING_VAULT is required (or deploy the proof system first)}"
    export PROPOSER_VAULT_TARGET="${PROPOSER_VAULT_TARGET:-100000000000000000000}"
    export CHALLENGER_VAULT_TARGET="${CHALLENGER_VAULT_TARGET:-100000000000000000000}"
    BROADCAST_FLAG=""
    if [ "{{dry_run}}" = "false" ]; then
        BROADCAST_FLAG="--broadcast"
    fi
    echo "WARNING: minting unrestricted mock WLD for proof-system bond accounts on '{{env}}'." >&2
    echo "  proposer $PROPOSER_ADDRESS target: $PROPOSER_VAULT_TARGET base units" >&2
    echo "  challenger $CHALLENGER_ADDRESS target: $CHALLENGER_VAULT_TARGET base units" >&2
    cd pkg/contracts && forge script scripts/devnet/FundProofBondAccounts.s.sol:FundProofBondAccounts \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" $BROADCAST_FLAG --slow

# Register and activate the WIP-1006 implementation after validating its wiring and current anchor.
# Set REQUIRE_FRESH_ANCHOR=true during a clean chain bootstrap.
[doc("Validate, register, and activate a WIP-1006 implementation.")]
[group('proof')]
proof-activate-system env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${DISPUTE_GAME_FACTORY:?DISPUTE_GAME_FACTORY is required}"
    : "${ANCHOR_STATE_REGISTRY:?ANCHOR_STATE_REGISTRY is required}"
    : "${SYSTEM_CONFIG:?SYSTEM_CONFIG is required}"
    : "${OP_CHAIN_PROXY_ADMIN:?OP_CHAIN_PROXY_ADMIN is required}"
    : "${WLD_TOKEN:?WLD_TOKEN is required}"
    : "${DGF_OWNER_KEY:?DGF_OWNER_KEY is required}"
    DEPLOYMENTS_FILE="pkg/contracts/deployments/{{env}}-proof-system.json"
    if [ -z "${GAME_IMPLEMENTATION:-}" ] && [ -f "$DEPLOYMENTS_FILE" ]; then
        GAME_IMPLEMENTATION=$(jq -r '.gameImplementation // empty' "$DEPLOYMENTS_FILE")
        export GAME_IMPLEMENTATION
    fi
    : "${GAME_IMPLEMENTATION:?GAME_IMPLEMENTATION is required (or deploy first)}"
    export REQUIRE_FRESH_ANCHOR="${REQUIRE_FRESH_ANCHOR:-false}"
    BROADCAST_FLAG=""
    if [ "{{dry_run}}" = "false" ]; then
        BROADCAST_FLAG="--broadcast"
    fi
    echo "Registering and activating WIP-1006 implementation $GAME_IMPLEMENTATION (require fresh anchor: $REQUIRE_FRESH_ANCHOR)$([ -n "$BROADCAST_FLAG" ] || echo ' [DRY RUN]')…" >&2
    cd pkg/contracts && forge script scripts/devnet/ActivateProofSystem.s.sol:ActivateProofSystem \
        --rpc-url "$L1_RPC_URL" --private-key "$DGF_OWNER_KEY" $BROADCAST_FLAG --slow

# Phase 3a – Pre-warm CertManager with the AWS Nitro CA cert chain.
#
# ATTESTATION_HEX is an input: fetch it with the infrastructure repo's
# `proof-attestation <env>` recipe, which execs into the worker pod (the only
# place with vsock access).
# ATTESTATION_FILE is accepted as an alternative, since the hex is large.
[doc("Phase 3a - Pre-warm CertManager with the AWS Nitro CA cert chain.")]
[group('proof')]
proof-certmanager-prewarm env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    # Fall back to the deployment file if CERT_MANAGER_ADDRESS is not set.
    DEPLOYMENTS_FILE="pkg/contracts/deployments/{{env}}-nitro.json"
    if [ -z "${CERT_MANAGER_ADDRESS:-}" ] && [ -f "$DEPLOYMENTS_FILE" ]; then
        CERT_MANAGER_ADDRESS=$(jq -r '.certManager' "$DEPLOYMENTS_FILE")
        export CERT_MANAGER_ADDRESS
    fi
    : "${CERT_MANAGER_ADDRESS:?CERT_MANAGER_ADDRESS is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    if [ -z "${ATTESTATION_HEX:-}" ] && [ -n "${ATTESTATION_FILE:-}" ]; then
        if [ ! -f "$ATTESTATION_FILE" ]; then
            echo "Error: ATTESTATION_FILE '$ATTESTATION_FILE' does not exist" >&2
            exit 1
        fi
        ATTESTATION_HEX=$(tr -d '[:space:]' < "$ATTESTATION_FILE")
    fi
    if [ -z "${ATTESTATION_HEX:-}" ]; then
        echo "Error: ATTESTATION_HEX (or ATTESTATION_FILE) is required." >&2
        echo "  Fetch it with the infrastructure repo's 'proof-attestation {{env}}' recipe." >&2
        exit 1
    fi
    echo "Generating pre-warm plan…"
    PREWARM_PLAN_RAW="/tmp/prewarm-plan-$$.json"
    PREWARM_PLAN="/tmp/prewarm-plan-$$-simple.json"
    trap 'rm -f "$PREWARM_PLAN_RAW" "$PREWARM_PLAN"' EXIT
    node pkg/contracts/lib/nitro-validator/tools/hinted_attestation_calls.js prepare \
        --attestation "$ATTESTATION_HEX" --cert-manager "$CERT_MANAGER_ADDRESS" \
        > "$PREWARM_PLAN_RAW"
    # Simplify to parallel arrays so vm.parseJsonStringArray works in the Forge script.
    # Filter out the validate_attestation entry (no certHash field).
    jq '{
      calldatas:  [.cold[] | select(.certHash != null) | .calldata],
      certHashes: [.cold[] | select(.certHash != null) | .certHash]
    }' "$PREWARM_PLAN_RAW" > "$PREWARM_PLAN"
    echo "Pre-warm plan saved to $PREWARM_PLAN"
    echo "Submitting cold cert entries via Forge script…"
    BROADCAST_FLAG=""
    PREWARM_SKIP_IF_UNDEPLOYED="false"
    if [ "{{dry_run}}" = "false" ]; then
        BROADCAST_FLAG="--broadcast"
    else
        PREWARM_SKIP_IF_UNDEPLOYED="true"
    fi
    cd pkg/contracts && CERT_MANAGER_ADDRESS="$CERT_MANAGER_ADDRESS" PREWARM_PLAN="$PREWARM_PLAN" \
        PREWARM_SKIP_IF_UNDEPLOYED="$PREWARM_SKIP_IF_UNDEPLOYED" \
        forge script scripts/devnet/PrewarmCertManager.s.sol:PrewarmCertManager \
            --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" $BROADCAST_FLAG --slow

# Phase 3b – Approve the PCR set on NitroAttestationVerifier.
[group('proof')]
proof-approve-pcrs env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    # Fall back to the deployment file if NITRO_ATTESTATION_VERIFIER is not set.
    DEPLOYMENTS_FILE="pkg/contracts/deployments/{{env}}-nitro.json"
    if [ -z "${NITRO_ATTESTATION_VERIFIER:-}" ] && [ -f "$DEPLOYMENTS_FILE" ]; then
        NITRO_ATTESTATION_VERIFIER=$(jq -r '.nitroAttestationVerifier' "$DEPLOYMENTS_FILE")
        export NITRO_ATTESTATION_VERIFIER
    fi
    : "${NITRO_ATTESTATION_VERIFIER:?NITRO_ATTESTATION_VERIFIER is required}"
    : "${OWNER_KEY:?OWNER_KEY is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${PCR0:?PCR0 is required (48-byte hex)}"
    : "${PCR1:?PCR1 is required (48-byte hex)}"
    : "${PCR2:?PCR2 is required (48-byte hex)}"
    echo "Approving PCR set on ${NITRO_ATTESTATION_VERIFIER}…"
    # PCR values must be 0x-prefixed hex so cast keccak hashes the raw bytes
    [[ "$PCR0" == 0x* ]] || PCR0="0x$PCR0"
    [[ "$PCR1" == 0x* ]] || PCR1="0x$PCR1"
    [[ "$PCR2" == 0x* ]] || PCR2="0x$PCR2"
    if [ "{{dry_run}}" = "true" ]; then
        echo "[DRY RUN] Estimating gas…"
        cast estimate "$NITRO_ATTESTATION_VERIFIER" \
            "approvePCRSet(bytes32,bytes32,bytes32)" \
            "$(cast keccak "$PCR0")" "$(cast keccak "$PCR1")" "$(cast keccak "$PCR2")" \
            --rpc-url "$L1_RPC_URL" --from "$(cast wallet address --private-key "$OWNER_KEY")"
    else
        cast send "$NITRO_ATTESTATION_VERIFIER" \
            "approvePCRSet(bytes32,bytes32,bytes32)" \
            "$(cast keccak "$PCR0")" "$(cast keccak "$PCR1")" "$(cast keccak "$PCR2")" \
            --rpc-url "$L1_RPC_URL" --private-key "$OWNER_KEY"
        # Record which measurements are actually approved on this verifier. DeployNitro only
        # writes addresses, so without this the deployment file cannot tell an approved
        # allowlist from an empty one — and an unprovisioned verifier rejects every
        # attestation with PCRSetNotApproved long after the deploy looks finished.
        if [ -f "$DEPLOYMENTS_FILE" ]; then
            TMP_DEPLOYMENTS="$(mktemp)"
            jq --arg v "$NITRO_ATTESTATION_VERIFIER" --arg p0 "$PCR0" --arg p1 "$PCR1" --arg p2 "$PCR2" \
                '.approvedPCRSets = ((.approvedPCRSets // []) + [{verifier: $v, pcr0: $p0, pcr1: $p1, pcr2: $p2}] | unique)' \
                "$DEPLOYMENTS_FILE" > "$TMP_DEPLOYMENTS" && mv "$TMP_DEPLOYMENTS" "$DEPLOYMENTS_FILE"
            echo "Recorded approved PCR set in $DEPLOYMENTS_FILE"
        fi
    fi
    echo "PCR set approved."
