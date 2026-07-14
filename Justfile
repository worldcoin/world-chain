set positional-arguments := true
set dotenv-load := true

# default recipe to display help information
default:
    @just --list

build:
    docker buildx build \
        --build-arg VERGEN_GIT_SHA="$(git rev-parse HEAD)" \
        -t world-chain:latest .

build-world-chain-bin:
    cargo build -p world-chain

devnet-up: build
    @just ./pkg/devnet/devnet-up

deploy-contracts:
    @just ./pkg/contracts/deploy-contracts

test *args='':
    RUST_LOG="info" cargo nextest run --workspace $@

# Test with flashblocks debug tracing
test-dev *args='':
    RUST_LOG="info,flashblocks=debug,world_chain=info" cargo nextest run --workspace $@

# Test with verbose flashblocks tracing (all subsystems at trace level)
test-verbose *args='':
    RUST_LOG="info,flashblocks=trace,world_chain=trace,bal_executor=trace,payload_builder=trace,engine::tree=trace" cargo nextest run --workspace $@

clippy:
    cargo +nightly clippy --workspace --all-targets --all-features

fmt: fmt-fix fmt-check contracts-fmt

contracts-fmt:
    @just ./pkg/contracts/fmt

fmt-fix:
    cargo +nightly fmt --all

fmt-check:
    cargo +nightly fmt --all -- --check

# Launch a local playground (in-process node swarm)
playground *args='':
    RUST_LOG="info" cargo run -p xtask --release -- launch-node $@

# Manage the native Rust HA devnet. Use `just devnet up -d` to run in the background and `just devnet down` to stop it.
# Set BAL=1 to enable flashblocks block access lists on the sequencer nodes.
devnet command='up' *args='':
    #!/usr/bin/env bash
    set -euo pipefail
    EXTRA_ARGS=()
    if [ "{{command}}" = "up" ]; then
        cargo build -p world-chain
        if [ "${BAL:-0}" = "1" ]; then
            EXTRA_ARGS+=(--bal-enabled)
        fi
    fi
    RUST_LOG="${RUST_LOG:-info,flashblocks=trace,engine_driver=info}" cargo run -p xtask -- devnet {{command}} {{args}} ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}

# Tail world-chain execution client logs from the running devnet (e.g. `just devnet-logs` or `just devnet-logs 0` for a specific sequencer).
devnet-logs index='':
    #!/usr/bin/env bash
    set -uo pipefail
    LOG_FILE="${WORLD_CHAIN_DEVNET_LOG_FILE:-target/devnet/logs/devnet.log}"
    if [ ! -f "$LOG_FILE" ]; then
        echo "no devnet log file at $LOG_FILE; is the devnet running?" >&2
        exit 1
    fi
    if [ -n "{{index}}" ]; then
        PATTERN="world-chain-el-{{index}} "
    else
        PATTERN="world-chain-el-"
    fi
    tail -n 200 -F "$LOG_FILE" | grep --line-buffered -- "$PATTERN"

# Run Contender stress tests against a running native Rust devnet.
stress *args='':
    @scripts/stress/stress.sh $@

# Prove a PBH transaction
prove *args='':
    cargo run -p xtask -- prove $@

# Compute the on-chain verification keys for the SP1 proof ELFs.
# The ELFs are compiled and embedded at build time by
# `proofs/succinct/elfs/build.rs` (sp1_build::build_program_with_args
# with docker:true at the pinned SP1 toolchain tag), so just running
# `cargo run` is enough — no separate ELF build step is required.
proof-vkeys *args='':
    cargo run --release -p world-chain-prover-sp1 -- vkeys $@

# Recompute vkeys from the embedded ELFs and update proofs/succinct/elf/vkeys.json.
# Requires Docker and the SP1 toolchain (sp1up v6.1.0) for reproducible ELF builds.
update-proof-vkeys:
    cargo run -p world-chain-prover-sp1 -- vkeys --output /tmp/vkeys-update.json
    jq -S . /tmp/vkeys-update.json > proofs/succinct/elf/vkeys.json

# Verify that the committed vkeys.json matches what the current source produces.
# Uses jq -S to normalize key ordering before comparing, so the diff is not
# sensitive to JSON insertion order. Used by CI. Fails if they differ.
verify-proof-vkeys:
    cargo run -p world-chain-prover-sp1 -- vkeys --output /tmp/vkeys-actual.json
    jq -S . proofs/succinct/elf/vkeys.json > /tmp/vkeys-committed.json
    jq -S . /tmp/vkeys-actual.json > /tmp/vkeys-actual-normalized.json
    diff /tmp/vkeys-committed.json /tmp/vkeys-actual-normalized.json || (echo "ERROR: vkeys.json is out of date. Run 'just update-proof-vkeys' to regenerate." && exit 1)

# Generate CLI reference docs for the mdbook
docs:
    cargo xtask docs

install *args='':
    cargo install --path bin/world-chain --locked $@

# ==============================================================================
# Proof System Deployment
# ==============================================================================
#
# env parameter selects a config file from scripts/proof-envs/<env>.env
# which sets KUBECONTEXT, PROOF_NAMESPACE, PROOF_NITRO_IMAGE, etc.
# Shell env vars override values from the config file.
# See scripts/proof-envs/README.md for details.
#
# Workflow phases:
#   Phase 0a  proof-rollup-config-hash   – Compute rollup config hash
#   Phase 0b  proof-get-attestation       – Fetch bare attestation doc from enclave
#   Phase 1   proof-deploy-nitro          – Deploy Nitro attestation contracts
#   Phase 2   proof-deploy-system         – Deploy proof system contracts
#   Phase 3a  proof-certmanager-prewarm   – Pre-warm CertManager with CA certs
#   Phase 3b  proof-approve-pcrs          – Approve PCR set on verifier
#   Combined  proof-setup                 – Run all phases in sequence
#
# Required env vars (varies by target):
#   PRIVATE_KEY, OWNER, OWNER_KEY, L1_RPC_URL,
#   WORLD_CHAIN_L2_CHAIN_ID, ROLLUP_CONFIG_HASH,
#   CERT_MANAGER_ADDRESS, NITRO_ATTESTATION_VERIFIER,
#   PCR0, PCR1, PCR2
#
# Optional (proof-rollup-config-hash — one of these, in priority order):
#   L2_RPC_URL, ROLLUP_CONFIG_URL, ROLLUP_CONFIG
# ==============================================================================

# Phase 0a – Compute and print the rollup config hash.
# Sources (checked in priority order):
#   L2_RPC_URL        – op-node RPC endpoint (port 9545, NOT the execution client on 8545)
#   ROLLUP_CONFIG_URL – URL to download the rollup config JSON from
#   ROLLUP_CONFIG     – local file path to an existing rollup config JSON
#   (default)         – auto port-forward to the op-node pod via kubectl
# L2_RPC_URL overrides auto port-forward; useful for CI or when already port-forwarded
proof-rollup-config-hash env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f "scripts/proof-envs/{{env}}.env" ]; then
        echo "Error: unknown env '{{env}}' — create scripts/proof-envs/{{env}}.env to configure it" >&2
        exit 1
    fi
    source scripts/proof-envs/{{env}}.env
    if [ -f "scripts/proof-envs/{{env}}.local.env" ]; then
        source scripts/proof-envs/{{env}}.local.env
    fi
    if [ -n "${L2_RPC_URL:-}" ]; then
        echo "Fetching rollup config from op-node at $L2_RPC_URL…" >&2
        cargo run -p world-chain-prover-sp1 -- hash-rollup-config --l2-rpc "$L2_RPC_URL"
    elif [ -n "${ROLLUP_CONFIG_URL:-}" ]; then
        echo "Downloading rollup config from $ROLLUP_CONFIG_URL…" >&2
        curl -sfSL "$ROLLUP_CONFIG_URL" -o /tmp/rollup.json
        cargo run -p world-chain-prover-sp1 -- hash-rollup-config --rollup-config /tmp/rollup.json
    elif [ -n "${ROLLUP_CONFIG:-}" ]; then
        echo "Using local rollup config: $ROLLUP_CONFIG" >&2
        cargo run -p world-chain-prover-sp1 -- hash-rollup-config --rollup-config "$ROLLUP_CONFIG"
    else
        LOCAL_PORT=19545
        echo "Port-forwarding to $OP_NODE_POD in $OP_NODE_NAMESPACE (context: $KUBECONTEXT)…" >&2
        kubectl --context="$KUBECONTEXT" port-forward \
            -n "$OP_NODE_NAMESPACE" \
            "pod/$OP_NODE_POD" "${LOCAL_PORT}:${OP_NODE_PORT}" &
        PF_PID=$!
        trap 'kill $PF_PID 2>/dev/null || true' EXIT
        for i in $(seq 1 10); do
            nc -z localhost $LOCAL_PORT 2>/dev/null && break || sleep 1
        done
        cargo run -p world-chain-prover-sp1 -- hash-rollup-config \
            --l2-rpc "http://localhost:$LOCAL_PORT"
    fi

# Phase 0b – Run a one-shot k8s Job to get a bare attestation doc from the enclave.
proof-get-attestation env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f "scripts/proof-envs/{{env}}.env" ]; then
        echo "Error: unknown env '{{env}}' — create scripts/proof-envs/{{env}}.env to configure it" >&2
        exit 1
    fi
    source scripts/proof-envs/{{env}}.env
    if [ -f "scripts/proof-envs/{{env}}.local.env" ]; then
        source scripts/proof-envs/{{env}}.local.env
    fi
    POD_NAME="proof-attestation-$(date +%s)"
    cleanup() { kubectl --context="$KUBECONTEXT" delete pod "$POD_NAME" -n "$PROOF_NAMESPACE" --ignore-not-found >&2; }
    trap cleanup EXIT
    echo "Spawning attestation pod $POD_NAME in namespace $PROOF_NAMESPACE (context: $KUBECONTEXT)…" >&2
    kubectl --context="$KUBECONTEXT" run "$POD_NAME" \
        --namespace "$PROOF_NAMESPACE" \
        --image "$PROOF_NITRO_IMAGE" \
        --restart=Never \
        --overrides='{
          "spec": {
            "nodeSelector": {"intent": "enclave"},
            "tolerations": [{"key": "enclave", "operator": "Exists", "effect": "NoExecute"}]
          }
        }' \
        -- get-attestation
    echo "Waiting for pod to complete…" >&2
    kubectl --context="$KUBECONTEXT" wait --for=condition=Ready pod/"$POD_NAME" -n "$PROOF_NAMESPACE" --timeout=120s 2>/dev/null || true
    kubectl --context="$KUBECONTEXT" wait --for=jsonpath='{.status.phase}'=Succeeded pod/"$POD_NAME" -n "$PROOF_NAMESPACE" --timeout=300s
    kubectl --context="$KUBECONTEXT" logs "$POD_NAME" -n "$PROOF_NAMESPACE"

# Phase 1 – Deploy the Nitro attestation stack.
proof-deploy-nitro:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${OWNER:?OWNER is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    export PROOF_DEPLOY_OUT="${PROOF_DEPLOY_OUT:-/tmp/nitro-deploy-$(date +%s).json}"
    echo "Deploying Nitro contracts (output → $PROOF_DEPLOY_OUT)…"
    cd pkg/contracts && forge script scripts/devnet/DeployNitro.s.sol:DeployNitro \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" --broadcast --slow

# Phase 2 – Deploy the proof system contracts.
proof-deploy-system:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${WORLD_CHAIN_L2_CHAIN_ID:?WORLD_CHAIN_L2_CHAIN_ID is required}"
    : "${ROLLUP_CONFIG_HASH:?ROLLUP_CONFIG_HASH is required}"
    export PROOF_SYSTEM_BLOCK_INTERVAL="${PROOF_SYSTEM_BLOCK_INTERVAL:-10}"
    export PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL="${PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL:-5}"
    export PROOF_THRESHOLD="${PROOF_THRESHOLD:-2}"
    export WORLD_CHALLENGER_ADDRESS="${WORLD_CHALLENGER_ADDRESS:-}"
    export PROOF_SYSTEM_DEPLOYMENT_OUT="${PROOF_SYSTEM_DEPLOYMENT_OUT:-/tmp/proof-system-deploy-$(date +%s).json}"
    echo "Deploying proof system contracts (output → $PROOF_SYSTEM_DEPLOYMENT_OUT)…"
    cd pkg/contracts && forge script scripts/devnet/DeployProofSystem.s.sol:DeployProofSystem \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" --broadcast --slow

# Phase 3a – Pre-warm CertManager with the AWS Nitro CA cert chain.
proof-certmanager-prewarm env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f "scripts/proof-envs/{{env}}.env" ]; then
        echo "Error: unknown env '{{env}}' — create scripts/proof-envs/{{env}}.env to configure it" >&2
        exit 1
    fi
    source scripts/proof-envs/{{env}}.env
    if [ -f "scripts/proof-envs/{{env}}.local.env" ]; then
        source scripts/proof-envs/{{env}}.local.env
    fi
    : "${CERT_MANAGER_ADDRESS:?CERT_MANAGER_ADDRESS is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    echo "Fetching attestation from enclave…"
    ATTESTATION_HEX=$(just proof-get-attestation {{env}})
    echo "Generating calldata…"
    CALLDATA_JSON=$(node pkg/contracts/lib/nitro-validator/tools/hinted_attestation_calls.js prepare \
        --attestation "$ATTESTATION_HEX" --cert-manager "$CERT_MANAGER_ADDRESS")
    COLD_ENTRIES=$(echo "$CALLDATA_JSON" | jq -r '.cold[]')
    if [ -z "$COLD_ENTRIES" ]; then
        echo "Error: no cold cert entries found — attestation may be invalid" >&2
        exit 1
    fi
    COUNT=$(echo "$COLD_ENTRIES" | wc -l)
    echo "Submitting $COUNT cold cert entries…"
    FAILED=0
    while IFS= read -r calldata; do
        echo "  Sending tx with calldata ${calldata:0:20}…"
        if ! cast send "$CERT_MANAGER_ADDRESS" \
            --data "$calldata" \
            --rpc-url "$L1_RPC_URL" \
            --private-key "$PRIVATE_KEY"; then
            echo "  Error: cast send failed for calldata ${calldata:0:20}" >&2
            FAILED=$((FAILED + 1))
        fi
    done <<< "$COLD_ENTRIES"
    if [ "$FAILED" -gt 0 ]; then
        echo "Error: $FAILED of $COUNT cold cert submissions failed" >&2
        exit 1
    fi
    echo "CertManager pre-warm complete ($COUNT entries submitted)."

# Phase 3b – Approve the PCR set on NitroAttestationVerifier.
proof-approve-pcrs:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${NITRO_ATTESTATION_VERIFIER:?NITRO_ATTESTATION_VERIFIER is required}"
    : "${OWNER_KEY:?OWNER_KEY is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${PCR0:?PCR0 is required (48-byte hex)}"
    : "${PCR1:?PCR1 is required (48-byte hex)}"
    : "${PCR2:?PCR2 is required (48-byte hex)}"
    echo "Approving PCR set on $NITRO_ATTESTATION_VERIFIER…"
    # PCR values must be 0x-prefixed hex so cast keccak hashes the raw bytes
    [[ "$PCR0" == 0x* ]] || PCR0="0x$PCR0"
    [[ "$PCR1" == 0x* ]] || PCR1="0x$PCR1"
    [[ "$PCR2" == 0x* ]] || PCR2="0x$PCR2"
    cast send "$NITRO_ATTESTATION_VERIFIER" \
        "approvePCRSet(bytes32,bytes32,bytes32)" \
        "$(cast keccak "$PCR0")" "$(cast keccak "$PCR1")" "$(cast keccak "$PCR2")" \
        --rpc-url "$L1_RPC_URL" --private-key "$OWNER_KEY"
    echo "PCR set approved."

# Combined – Run all proof system deployment phases in sequence.
# NOTE: proof-deploy-nitro outputs contract addresses (CERT_MANAGER_ADDRESS,
# NITRO_ATTESTATION_VERIFIER) that must be set before phases 3a/3b.
# Export them in your shell between steps, or run phases individually.
proof-setup env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f "scripts/proof-envs/{{env}}.env" ]; then
        echo "Error: unknown env '{{env}}' — create scripts/proof-envs/{{env}}.env to configure it" >&2
        exit 1
    fi
    # Validate that downstream env vars are set before starting
    : "${CERT_MANAGER_ADDRESS:?CERT_MANAGER_ADDRESS is required (from proof-deploy-nitro output)}"
    : "${NITRO_ATTESTATION_VERIFIER:?NITRO_ATTESTATION_VERIFIER is required (from proof-deploy-nitro output)}"
    just proof-deploy-nitro
    just proof-deploy-system
    just proof-certmanager-prewarm {{env}}
    just proof-approve-pcrs
