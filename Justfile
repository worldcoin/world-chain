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
    cargo +nightly-2026-07-01 clippy --workspace --all-targets --all-features

fmt: fmt-fix fmt-check contracts-fmt

contracts-fmt:
    @just ./pkg/contracts/fmt

fmt-fix:
    cargo +nightly-2026-07-01 fmt --all

fmt-check:
    cargo +nightly-2026-07-01 fmt --all -- --check

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
#   Phase 0a  proof-get-chain-id          – Print the L2 chain ID from the op-node
#   Phase 0b  proof-get-attestation       – Fetch bare attestation doc from enclave
#   Phase 0b  proof-get-pcrs              – Print PCR0/PCR1/PCR2 from the EIF on the enclave-launcher container
#   Phase 1   proof-deploy-nitro          – Deploy Nitro attestation contracts
#   Phase 2   proof-deploy-system         – Deploy proof system contracts
#   Phase 3a  proof-certmanager-prewarm   – Pre-warm CertManager with CA certs
#   Phase 3b  proof-approve-pcrs          – Approve PCR set on verifier
#   Combined  proof-setup                 – Run all phases in sequence
#
# Required env vars (varies by target):
#   PRIVATE_KEY, OWNER, OWNER_KEY, L1_RPC_URL,
#   WORLD_CHAIN_L2_CHAIN_ID, ROLLUP_CONFIG_HASH,
#   CERT_MANAGER_ADDRESS, NITRO_ATTESTATION_VERIFIER
#
# Optional env vars (auto-fetched from enclave if not set):
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
        READY=false
        for i in $(seq 1 10); do
            if nc -z localhost "$LOCAL_PORT" 2>/dev/null; then
                READY=true
                break
            fi
            # check that the port-forward process is still alive
            if ! kill -0 "$PF_PID" 2>/dev/null; then
                echo "Error: kubectl port-forward exited unexpectedly" >&2
                exit 1
            fi
            sleep 1
        done
        if [ "$READY" != true ]; then
            echo "Error: port-forward to localhost:$LOCAL_PORT not ready after 10s" >&2
            exit 1
        fi
        cargo run -p world-chain-prover-sp1 -- hash-rollup-config \
            --l2-rpc "http://localhost:$LOCAL_PORT"
    fi

# Phase 0a (alt) – Print the L2 chain ID from the op-node rollup config.
#                   Uses the same port-forward pattern as proof-rollup-config-hash.
#                   If L2_CHAIN_ID is already set, prints it directly.
#                   Output: bare integer, e.g. 480
proof-get-chain-id env="alphanet":
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
    if [ -n "${L2_CHAIN_ID:-}" ]; then
        echo "$L2_CHAIN_ID"
        exit 0
    fi
    if [ -n "${L2_RPC_URL:-}" ]; then
        RPC_URL="$L2_RPC_URL"
    else
        LOCAL_PORT=19546
        echo "Port-forwarding to $OP_NODE_POD in $OP_NODE_NAMESPACE (context: $KUBECONTEXT)…" >&2
        kubectl --context="$KUBECONTEXT" port-forward \
            -n "$OP_NODE_NAMESPACE" \
            "pod/$OP_NODE_POD" "${LOCAL_PORT}:${OP_NODE_PORT}" &
        PF_PID=$!
        trap 'kill $PF_PID 2>/dev/null || true' EXIT
        READY=false
        for i in $(seq 1 10); do
            if nc -z localhost "$LOCAL_PORT" 2>/dev/null; then
                READY=true
                break
            fi
            if ! kill -0 "$PF_PID" 2>/dev/null; then
                echo "Error: kubectl port-forward exited unexpectedly" >&2
                exit 1
            fi
            sleep 1
        done
        if [ "$READY" != true ]; then
            echo "Error: port-forward to localhost:$LOCAL_PORT not ready after 10s" >&2
            exit 1
        fi
        RPC_URL="http://localhost:$LOCAL_PORT"
    fi
    cast rpc --rpc-url "$RPC_URL" optimism_rollupConfig 2>/dev/null \
        | jq -r '.l2ChainId // .l2_chain_id'

# Phase 0b  – Fetch a bare attestation doc from the running Nitro enclave.
#              Execs into the nitro-worker pod (which already has vsock device access)
#              and calls `nitro-worker get-attestation`. Prints hex attestation to stdout.
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
    NITRO_POD=$(kubectl --context="$KUBECONTEXT" get pod \
        -n "$PROOF_NAMESPACE" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -z "$NITRO_POD" ]; then
        echo "Error: no running pod found in namespace $PROOF_NAMESPACE" >&2
        exit 1
    fi
    # Get the name of the main (non-init) container — first container in the pod spec
    CONTAINER=$(kubectl --context="$KUBECONTEXT" get pod "$NITRO_POD" \
        -n "$PROOF_NAMESPACE" \
        -o jsonpath='{.spec.containers[0].name}')
    # Check it is actually Running
    CONTAINER_STATE=$(kubectl --context="$KUBECONTEXT" get pod "$NITRO_POD" \
        -n "$PROOF_NAMESPACE" \
        -o jsonpath="{.status.containerStatuses[?(@.name==\"$CONTAINER\")].state.running}")
    if [ -z "$CONTAINER_STATE" ]; then
        echo "Error: container '$CONTAINER' in pod '$NITRO_POD' is not in Running state" >&2
        kubectl --context="$KUBECONTEXT" get pod "$NITRO_POD" -n "$PROOF_NAMESPACE" >&2
        exit 1
    fi
    ENCLAVE_CID=$(kubectl --context="$KUBECONTEXT" exec \
        -n "$PROOF_NAMESPACE" "$NITRO_POD" -c "$CONTAINER" \
        -- cat /run/nitro-shared/enclave-cid 2>/dev/null || echo "16")
    echo "Pod: $NITRO_POD  Container: $CONTAINER  CID: $ENCLAVE_CID" >&2
    kubectl --context="$KUBECONTEXT" exec \
        -n "$PROOF_NAMESPACE" "$NITRO_POD" -c "$CONTAINER" \
        -- sh -c "ENCLAVE_CID=$ENCLAVE_CID nitro-worker get-attestation"

# Phase 0b (alt) – Print PCR0, PCR1, PCR2 from the EIF image on the enclave-launcher container.
proof-get-pcrs env="alphanet":
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
    NITRO_POD=$(kubectl --context="$KUBECONTEXT" get pod \
        -n "$PROOF_NAMESPACE" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -z "$NITRO_POD" ]; then
        echo "Error: no running pod found in namespace $PROOF_NAMESPACE" >&2
        exit 1
    fi
    echo "Pod: $NITRO_POD  Container: enclave-launcher" >&2
    MEASUREMENTS=$(kubectl --context="$KUBECONTEXT" exec \
        -n "$PROOF_NAMESPACE" "$NITRO_POD" -c enclave-launcher \
        -- nitro-cli describe-eif --eif-path /home/world-chain-nitro-worker-enclave.eif \
        | jq -r '.Measurements')
    echo "PCR0=$(echo "$MEASUREMENTS" | jq -r '.PCR0')"
    echo "PCR1=$(echo "$MEASUREMENTS" | jq -r '.PCR1')"
    echo "PCR2=$(echo "$MEASUREMENTS" | jq -r '.PCR2')"

# Phase 1 – Deploy the Nitro attestation stack.
proof-deploy-nitro:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${OWNER:?OWNER is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    PROOF_DEPLOY_OUT="${PROOF_DEPLOY_OUT:-/tmp/nitro-deploy-$(date +%s).log}"
    echo "Deploying Nitro contracts (output → $PROOF_DEPLOY_OUT)…"
    cd pkg/contracts && forge script scripts/devnet/DeployNitro.s.sol:DeployNitro \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" --broadcast --slow \
        | tee "$PROOF_DEPLOY_OUT"

# Phase 2 – Deploy the proof system contracts.
# Devnet-only deployment using the stock OP DisputeGameFactory, AnchorStateRegistry,
# SystemConfig, and chain ProxyAdmin deployed by op-deployer.
proof-deploy-system:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${WORLD_CHAIN_L2_CHAIN_ID:?WORLD_CHAIN_L2_CHAIN_ID is required}"
    : "${ROLLUP_CONFIG_HASH:?ROLLUP_CONFIG_HASH is required}"
    : "${DISPUTE_GAME_FACTORY:?DISPUTE_GAME_FACTORY is required (op-deployer DisputeGameFactoryProxy)}"
    : "${ANCHOR_STATE_REGISTRY:?ANCHOR_STATE_REGISTRY is required (op-deployer AnchorStateRegistryProxy)}"
    : "${SYSTEM_CONFIG:?SYSTEM_CONFIG is required (op-deployer SystemConfigProxy)}"
    : "${OP_CHAIN_PROXY_ADMIN:?OP_CHAIN_PROXY_ADMIN is required (op-deployer ProxyAdmin)}"
    : "${OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY:?OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY is required}"
    : "${DGF_OWNER_KEY:?DGF_OWNER_KEY is required}"
    : "${GUARDIAN_KEY:?GUARDIAN_KEY is required}"
    export PROOF_SYSTEM_BLOCK_INTERVAL="${PROOF_SYSTEM_BLOCK_INTERVAL:-10}"
    export PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL="${PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL:-5}"
    export PROOF_THRESHOLD="${PROOF_THRESHOLD:-2}"
    export WORLD_CHALLENGER_ADDRESS="${WORLD_CHALLENGER_ADDRESS:-}"
    export DELAYED_WETH_DELAY="${DELAYED_WETH_DELAY:-300}"
    export SET_RESPECTED_GAME_TYPE="${SET_RESPECTED_GAME_TYPE:-true}"
    export PROOF_SYSTEM_DEPLOYMENT_OUT="${PROOF_SYSTEM_DEPLOYMENT_OUT:-/tmp/proof-system-deploy-$(date +%s).json}"
    echo "Deploying proof system contracts (output → $PROOF_SYSTEM_DEPLOYMENT_OUT)…"
    cd pkg/contracts && just build-opstack && forge script scripts/devnet/DeployProofSystem.s.sol:DeployProofSystem \
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
    cd pkg/contracts && CERT_MANAGER_ADDRESS="$CERT_MANAGER_ADDRESS" PREWARM_PLAN="$PREWARM_PLAN" \
        forge script scripts/devnet/PrewarmCertManager.s.sol:PrewarmCertManager \
            --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" --broadcast --slow

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
    echo "Approving PCR set on ${NITRO_ATTESTATION_VERIFIER}…"
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
# Automatically wires contract addresses between steps. PCR0/1/2 are
# auto-fetched from the running enclave if not pre-set.
proof-setup env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f "scripts/proof-envs/{{env}}.env" ]; then
        echo "Error: unknown env '{{env}}' — create scripts/proof-envs/{{env}}.env to configure it" >&2
        exit 1
    fi
    if [ -z "${WORLD_CHAIN_L2_CHAIN_ID:-}" ]; then
        echo "=== Step 0-pre: Fetching L2 chain ID from op-node ===" >&2
        WORLD_CHAIN_L2_CHAIN_ID=$(just proof-get-chain-id {{env}})
        export WORLD_CHAIN_L2_CHAIN_ID
        echo "WORLD_CHAIN_L2_CHAIN_ID=$WORLD_CHAIN_L2_CHAIN_ID" >&2
    fi

    echo "=== Step 0: Computing rollup config hash ===" >&2
    ROLLUP_CONFIG_HASH=$(just proof-rollup-config-hash {{env}})
    export ROLLUP_CONFIG_HASH
    echo "ROLLUP_CONFIG_HASH=$ROLLUP_CONFIG_HASH" >&2

    echo "=== Step 1: Deploying Nitro attestation stack ===" >&2
    NITRO_LOG=$(mktemp)
    just proof-deploy-nitro | tee "$NITRO_LOG"
    CERT_MANAGER_ADDRESS=$(grep -oP 'CertManager:\s+\K0x[0-9a-fA-F]{40}' "$NITRO_LOG")
    NITRO_ATTESTATION_VERIFIER=$(grep -oP 'NitroAttestationVerifier:\s+\K0x[0-9a-fA-F]{40}' "$NITRO_LOG")
    export CERT_MANAGER_ADDRESS NITRO_ATTESTATION_VERIFIER
    echo "CERT_MANAGER_ADDRESS=$CERT_MANAGER_ADDRESS" >&2
    echo "NITRO_ATTESTATION_VERIFIER=$NITRO_ATTESTATION_VERIFIER" >&2
    rm -f "$NITRO_LOG"

    echo "=== Step 2: Deploying proof system contracts ===" >&2
    just proof-deploy-system

    echo "=== Step 3a: Pre-warming CertManager ===" >&2
    just proof-certmanager-prewarm {{env}}

    if [ -z "${PCR0:-}" ] || [ -z "${PCR1:-}" ] || [ -z "${PCR2:-}" ]; then
        echo "=== Step 3b-pre: Fetching PCRs from running enclave ===" >&2
        eval $(just proof-get-pcrs {{env}})
    fi

    echo "=== Step 3b: Approving PCR set ===" >&2
    just proof-approve-pcrs
