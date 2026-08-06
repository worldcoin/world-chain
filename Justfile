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

# Build the pinned Optimism implementation contracts in the isolated opstack/ sub-project.
build-opstack:
    @just ./pkg/contracts/build-opstack

build-contracts *args='':
    @just ./pkg/contracts/build-contracts $@

test-contracts *args='':
    @just ./pkg/contracts/test-contracts $@

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
#   Phase 3c  proof-verify-pcrs           – Assert the RUNNING enclave's PCR set is approved
#                                            (drift check; safe to run any time)
#   Phase 4   proof-register-key          – Register the enclave's generated key on-chain
#                                            (run separately; NOT part of proof-setup)
#   Combined  proof-setup                 – Run deploy phases 0a–3c in sequence (does NOT
#                                            run Phase 4 — register the key afterwards with
#                                            proof-register-key, or let the worker
#                                            self-register via `nitro-worker run --auto-register`)
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
#
# Simulation mode (no on-chain broadcast):
#   dry_run=true   Simulate without broadcasting (all other steps still run)
# ==============================================================================

# Set dry_run=true on the command line to simulate without broadcasting.
dry_run := "false"

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
        cargo run -p world-chain-prover-nitro -- hash-rollup-config --l2-rpc "$L2_RPC_URL"
    elif [ -n "${ROLLUP_CONFIG_URL:-}" ]; then
        echo "Downloading rollup config from $ROLLUP_CONFIG_URL…" >&2
        curl -sfSL "$ROLLUP_CONFIG_URL" -o /tmp/rollup.json
        cargo run -p world-chain-prover-nitro -- hash-rollup-config --rollup-config /tmp/rollup.json
    elif [ -n "${ROLLUP_CONFIG:-}" ]; then
        echo "Using local rollup config: $ROLLUP_CONFIG" >&2
        cargo run -p world-chain-prover-nitro -- hash-rollup-config --rollup-config "$ROLLUP_CONFIG"
    else
        LOCAL_PORT=19545
        echo "Port-forwarding to $OP_NODE_POD in $OP_NODE_NAMESPACE (context: $KUBECONTEXT)…" >&2
        kubectl --context="$KUBECONTEXT" port-forward \
            -n "$OP_NODE_NAMESPACE" \
            "pod/$OP_NODE_POD" "${LOCAL_PORT}:${OP_NODE_PORT}" > /dev/null 2>&1 &
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
        cargo run -p world-chain-prover-nitro -- hash-rollup-config \
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
            "pod/$OP_NODE_POD" "${LOCAL_PORT}:${OP_NODE_PORT}" > /dev/null 2>&1 &
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
        -- nitro-cli describe-eif --eif-path /home/world-chain-nitro-enclave.eif \
        | jq -r '.Measurements')
    echo "PCR0=$(echo "$MEASUREMENTS" | jq -r '.PCR0')"
    echo "PCR1=$(echo "$MEASUREMENTS" | jq -r '.PCR1')"
    echo "PCR2=$(echo "$MEASUREMENTS" | jq -r '.PCR2')"

# Phase 1 – Deploy the Nitro attestation stack.
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
proof-deploy-mocks env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PRIVATE_KEY:?PRIVATE_KEY is required}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    export WORLD_CHALLENGER_ADDRESS="${WORLD_CHALLENGER_ADDRESS:-}"
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
    echo "WARNING: deploying MOCK proof verifiers (accept any proof) for '{{env}}'." >&2
    echo "Deploying proof mocks (deployment → $PROOF_MOCKS_DEPLOYMENT_OUT)$([ -n "$BROADCAST_FLAG" ] || echo ' [DRY RUN]')…"
    cd pkg/contracts && mkdir -p deployments && forge script scripts/devnet/DeployProofMocks.s.sol:DeployProofMocks \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" $BROADCAST_FLAG --slow

# The three proof-lane verifiers and the staking registry are required inputs — this
# script never deploys them, and rejects addresses that hold no code or that repeat
# across lanes. Point them at real contracts; for a devnet, run `proof-deploy-mocks`
# first and read the four addresses out of deployments/<env>-proof-mocks.json.
# Phase 2 – Deploy the proof system contracts and register game type 1006.
proof-deploy-system env="alphanet":
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
    : "${PROTOCOL_FEE_RECIPIENT:?PROTOCOL_FEE_RECIPIENT is required (challenge-fee proceeds)}"
    : "${VALIDITY_PROOF_VERIFIER:?VALIDITY_PROOF_VERIFIER is required (e.g. SP1ValidityVerifier; devnet: proof-deploy-mocks)}"
    : "${TEE_VERIFIER:?TEE_VERIFIER is required (e.g. NitroProofVerifier from proof-deploy-nitro)}"
    : "${SECURITY_COUNCIL_VERIFIER:?SECURITY_COUNCIL_VERIFIER is required (council attestation verifier)}"
    : "${STAKING_REGISTRY:?STAKING_REGISTRY is required (IWorldChainStakingRegistry implementation)}"
    export PROOF_SYSTEM_BLOCK_INTERVAL="${PROOF_SYSTEM_BLOCK_INTERVAL:-10}"
    export PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL="${PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL:-5}"
    export CHALLENGE_PERIOD="${CHALLENGE_PERIOD:-86400}"
    export PROOF_PERIOD="${PROOF_PERIOD:-604800}"
    export PROPOSER_BOND="${PROPOSER_BOND:-10000000000000000}"
    export CHALLENGER_BOND="${CHALLENGER_BOND:-1000000000000000}"
    export CHALLENGE_FEE="${CHALLENGE_FEE:-100000000000000}"
    export PROOF_THRESHOLD="${PROOF_THRESHOLD:-2}"
    export DELAYED_WETH_DELAY="${DELAYED_WETH_DELAY:-300}"
    BROADCAST_FLAG=""
    if [ "{{dry_run}}" = "false" ]; then
        BROADCAST_FLAG="--broadcast"
    fi
    echo "Proof-system parameters:" >&2
    echo "  challenge period: $CHALLENGE_PERIOD seconds" >&2
    echo "  proof period: $PROOF_PERIOD seconds" >&2
    echo "  proposer bond: $PROPOSER_BOND wei" >&2
    echo "  challenger bond: $CHALLENGER_BOND wei" >&2
    echo "  challenge fee: $CHALLENGE_FEE wei" >&2
    # A dry run must never overwrite the record of a live deployment: the simulated game and
    # WETH addresses are never deployed, so writing them to the real path silently replaces a
    # true record with fictional addresses.
    if [ -n "$BROADCAST_FLAG" ]; then
        export PROOF_SYSTEM_DEPLOYMENT_OUT="deployments/{{env}}-proof-system.json"
    else
        export PROOF_SYSTEM_DEPLOYMENT_OUT="deployments/{{env}}-proof-system.dryrun.json"
    fi
    echo "Deploying proof system contracts (deployment → $PROOF_SYSTEM_DEPLOYMENT_OUT)$([ -n "$BROADCAST_FLAG" ] || echo ' [DRY RUN]')…"
    cd pkg/contracts && mkdir -p deployments && forge script scripts/devnet/DeployProofSystem.s.sol:DeployProofSystem \
        --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY" $BROADCAST_FLAG --slow

# Activate the registered WIP-1006 implementation after validating its wiring and current anchor.
# Set REQUIRE_FRESH_ANCHOR=true during a clean chain bootstrap.
proof-activate-system env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    : "${DISPUTE_GAME_FACTORY:?DISPUTE_GAME_FACTORY is required}"
    : "${ANCHOR_STATE_REGISTRY:?ANCHOR_STATE_REGISTRY is required}"
    : "${SYSTEM_CONFIG:?SYSTEM_CONFIG is required}"
    : "${GUARDIAN_KEY:?GUARDIAN_KEY is required}"
    export REQUIRE_FRESH_ANCHOR="${REQUIRE_FRESH_ANCHOR:-false}"
    BROADCAST_FLAG=""
    if [ "{{dry_run}}" = "false" ]; then
        BROADCAST_FLAG="--broadcast"
    fi
    echo "Activating WIP-1006 (require fresh anchor: $REQUIRE_FRESH_ANCHOR)$([ -n "$BROADCAST_FLAG" ] || echo ' [DRY RUN]')…" >&2
    cd pkg/contracts && forge script scripts/devnet/ActivateProofSystem.s.sol:ActivateProofSystem \
        --rpc-url "$L1_RPC_URL" --private-key "$GUARDIAN_KEY" $BROADCAST_FLAG --slow

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
    # Fall back to the deployment file if CERT_MANAGER_ADDRESS is not set.
    DEPLOYMENTS_FILE="pkg/contracts/deployments/{{env}}-nitro.json"
    if [ -z "${CERT_MANAGER_ADDRESS:-}" ] && [ -f "$DEPLOYMENTS_FILE" ]; then
        CERT_MANAGER_ADDRESS=$(jq -r '.certManager' "$DEPLOYMENTS_FILE")
        export CERT_MANAGER_ADDRESS
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

# Phase 3c – Verify the RUNNING enclave's PCR set is approved on-chain.
#
# Every EIF rollout changes PCR0 (enclave image) and PCR2 (application) while PCR1
# (kernel) stays put. Nothing re-approves the new measurements automatically, so the
# running enclave silently drops off the on-chain allowlist and every registerKey / TEE
# proof reverts.
#
# This measures the enclave that is actually running rather than trusting PCR0/1/2 from
# the shell — trusting the shell would reproduce exactly the drift this is meant to catch.
#
# Required: L1_RPC_URL. Optional: NITRO_ATTESTATION_VERIFIER (else read from the
#           {{env}}-nitro.json deployment).
proof-verify-pcrs env="alphanet":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f "scripts/proof-envs/{{env}}.env" ]; then
        echo "Error: unknown env '{{env}}' — create scripts/proof-envs/{{env}}.env to configure it" >&2
        exit 1
    fi
    DEPLOYMENTS_FILE="pkg/contracts/deployments/{{env}}-nitro.json"
    if [ -z "${NITRO_ATTESTATION_VERIFIER:-}" ] && [ -f "$DEPLOYMENTS_FILE" ]; then
        NITRO_ATTESTATION_VERIFIER=$(jq -r '.nitroAttestationVerifier // empty' "$DEPLOYMENTS_FILE")
    fi
    : "${NITRO_ATTESTATION_VERIFIER:?NITRO_ATTESTATION_VERIFIER is required (set it or run proof-deploy-nitro first)}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"

    echo "Measuring the running enclave…" >&2
    eval "$(just proof-get-pcrs {{env}})"
    : "${PCR0:?proof-get-pcrs did not return PCR0}"
    : "${PCR1:?proof-get-pcrs did not return PCR1}"
    : "${PCR2:?proof-get-pcrs did not return PCR2}"
    [[ "$PCR0" == 0x* ]] || PCR0="0x$PCR0"
    [[ "$PCR1" == 0x* ]] || PCR1="0x$PCR1"
    [[ "$PCR2" == 0x* ]] || PCR2="0x$PCR2"

    # The verifier stores keccak256 of each raw 48-byte PCR, not the PCR itself.
    APPROVED=$(cast call "$NITRO_ATTESTATION_VERIFIER" \
        "isPCRSetApproved(bytes32,bytes32,bytes32)(bool)" \
        "$(cast keccak "$PCR0")" "$(cast keccak "$PCR1")" "$(cast keccak "$PCR2")" \
        --rpc-url "$L1_RPC_URL")

    if [ "$APPROVED" != "true" ]; then
        echo "" >&2
        echo "ERROR: the running enclave's PCR set is NOT approved on ${NITRO_ATTESTATION_VERIFIER}." >&2
        echo "" >&2
        echo "  running enclave:" >&2
        echo "    PCR0=$PCR0" >&2
        echo "    PCR1=$PCR1" >&2
        echo "    PCR2=$PCR2" >&2
        if [ -f "$DEPLOYMENTS_FILE" ]; then
            echo "  approved in $DEPLOYMENTS_FILE:" >&2
            jq -r '.approvedPCRSets[]? | "    PCR0=\(.pcr0)\n    PCR1=\(.pcr1)\n    PCR2=\(.pcr2)"' \
                "$DEPLOYMENTS_FILE" >&2 || true
        fi
        echo "" >&2
        echo "Until this is approved, registerKey and every TEE proof will revert." >&2
        echo "Fix: OWNER_KEY=... just proof-approve-pcrs {{env}}" >&2
        exit 1
    fi
    echo "Running enclave's PCR set is approved on ${NITRO_ATTESTATION_VERIFIER}."

# Phase 4 – Register the enclave's generated signing key on-chain.
#            Execs into the running nitro-worker pod (which has vsock access to the
#            enclave) and runs `nitro-worker register`, which fetches a public-key
#            attestation, builds the registerKey calldata (with P-384 hints) and submits
#            it to NitroEnclaveKeyRegistry. Idempotent: a no-op if already registered.
#            `registerKey` is NOT owner-gated, so any funded key works.
#
# Required: L1_RPC_URL, and a funding key via REGISTER_PRIVATE_KEY or PRIVATE_KEY.
# Optional: NITRO_ENCLAVE_KEY_REGISTRY (else read from the {{env}}-nitro.json deployment),
#           PCR0/PCR1/PCR2 (else host-side attestation checks are skipped; the on-chain
#           verifier still enforces the approved PCR allowlist).
proof-register-key env="alphanet":
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
    # Resolve the registry address from the env or the deployment file.
    DEPLOYMENTS_FILE="pkg/contracts/deployments/{{env}}-nitro.json"
    if [ -z "${NITRO_ENCLAVE_KEY_REGISTRY:-}" ] && [ -f "$DEPLOYMENTS_FILE" ]; then
        # `// empty` so a missing/null key yields "" (not the literal "null"), which the
        # required-var check below then rejects with a clear message.
        NITRO_ENCLAVE_KEY_REGISTRY=$(jq -r '.nitroEnclaveKeyRegistry // empty' "$DEPLOYMENTS_FILE")
    fi
    : "${NITRO_ENCLAVE_KEY_REGISTRY:?NITRO_ENCLAVE_KEY_REGISTRY is required (set it or run proof-deploy-nitro first)}"
    : "${L1_RPC_URL:?L1_RPC_URL is required}"
    REGISTER_KEY="${REGISTER_PRIVATE_KEY:-${PRIVATE_KEY:-}}"
    : "${REGISTER_KEY:?set REGISTER_PRIVATE_KEY or PRIVATE_KEY (any funded key — registerKey is not owner-gated)}"
    NITRO_POD=$(kubectl --context="$KUBECONTEXT" get pod \
        -n "$PROOF_NAMESPACE" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -z "$NITRO_POD" ]; then
        echo "Error: no running pod found in namespace $PROOF_NAMESPACE" >&2
        exit 1
    fi
    CONTAINER=$(kubectl --context="$KUBECONTEXT" get pod "$NITRO_POD" \
        -n "$PROOF_NAMESPACE" \
        -o jsonpath='{.spec.containers[0].name}')
    # Check the container is actually Running before we exec (and pipe the funding key) in.
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
    echo "Pod: $NITRO_POD  Container: $CONTAINER  CID: $ENCLAVE_CID  Registry: $NITRO_ENCLAVE_KEY_REGISTRY" >&2
    # Pass everything (including the funding key) over STDIN rather than as `sh -c`
    # arguments, so secrets never appear in the container argv / kubectl audit logs, and
    # shell metacharacters in any value can't break out. Each value is single-quoted with
    # embedded single quotes escaped.
    shq() { printf "'%s'" "$(printf '%s' "${1:-}" | sed "s/'/'\\\\''/g")"; }
    {
        printf 'export ENCLAVE_CID=%s\n' "$(shq "$ENCLAVE_CID")"
        printf 'export NITRO_ENCLAVE_KEY_REGISTRY=%s\n' "$(shq "$NITRO_ENCLAVE_KEY_REGISTRY")"
        printf 'export L1_RPC_URL=%s\n' "$(shq "$L1_RPC_URL")"
        printf 'export REGISTER_PRIVATE_KEY=%s\n' "$(shq "$REGISTER_KEY")"
        if [ -n "${PCR0:-}" ]; then printf 'export PCR0=%s\n' "$(shq "$PCR0")"; fi
        if [ -n "${PCR1:-}" ]; then printf 'export PCR1=%s\n' "$(shq "$PCR1")"; fi
        if [ -n "${PCR2:-}" ]; then printf 'export PCR2=%s\n' "$(shq "$PCR2")"; fi
        printf 'exec nitro-worker register\n'
    } | kubectl --context="$KUBECONTEXT" exec -i \
        -n "$PROOF_NAMESPACE" "$NITRO_POD" -c "$CONTAINER" -- sh -s

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
    just dry_run={{dry_run}} proof-deploy-nitro {{env}}
    NITRO_DEPLOYMENTS="pkg/contracts/deployments/{{env}}-nitro.json"
    CERT_MANAGER_ADDRESS=$(jq -r '.certManager' "$NITRO_DEPLOYMENTS")
    NITRO_ATTESTATION_VERIFIER=$(jq -r '.nitroAttestationVerifier' "$NITRO_DEPLOYMENTS")
    export CERT_MANAGER_ADDRESS NITRO_ATTESTATION_VERIFIER
    echo "CERT_MANAGER_ADDRESS=$CERT_MANAGER_ADDRESS" >&2
    echo "NITRO_ATTESTATION_VERIFIER=$NITRO_ATTESTATION_VERIFIER" >&2

    # The TEE lane uses the real Nitro verifier deployed in Step 1.
    : "${TEE_VERIFIER:=$(jq -r '.nitroProofVerifier // empty' "$NITRO_DEPLOYMENTS")}"
    export TEE_VERIFIER
    : "${TEE_VERIFIER:?could not resolve nitroProofVerifier from $NITRO_DEPLOYMENTS}"
    echo "TEE_VERIFIER=$TEE_VERIFIER (real Nitro verifier)" >&2

    # Any lane without a real verifier falls back to a test double, deployed explicitly
    # here rather than silently inside proof-deploy-system. Each unset lane is named so a
    # mocked deployment is obvious in the log.
    if [ -z "${VALIDITY_PROOF_VERIFIER:-}" ] || [ -z "${SECURITY_COUNCIL_VERIFIER:-}" ] \
       || [ -z "${STAKING_REGISTRY:-}" ]; then
        echo "=== Step 1b: Deploying test doubles for unset lanes ===" >&2
        for v in VALIDITY_PROOF_VERIFIER SECURITY_COUNCIL_VERIFIER STAKING_REGISTRY; do
            eval "val=\${$v:-}"
            [ -z "$val" ] && echo "  MOCKED: $v" >&2
        done
        just dry_run={{dry_run}} proof-deploy-mocks {{env}}
        MOCKS="pkg/contracts/deployments/{{env}}-proof-mocks.json"
        : "${VALIDITY_PROOF_VERIFIER:=$(jq -r '.validityProofVerifier' "$MOCKS")}"
        : "${SECURITY_COUNCIL_VERIFIER:=$(jq -r '.securityCouncil' "$MOCKS")}"
        : "${STAKING_REGISTRY:=$(jq -r '.stakingRegistry' "$MOCKS")}"
    fi
    export VALIDITY_PROOF_VERIFIER SECURITY_COUNCIL_VERIFIER STAKING_REGISTRY

    echo "=== Step 2: Deploying proof system contracts ===" >&2
    just dry_run={{dry_run}} proof-deploy-system {{env}}

    echo "=== Step 3a: Pre-warming CertManager ===" >&2
    just dry_run={{dry_run}} proof-certmanager-prewarm {{env}}

    if [ -z "${PCR0:-}" ] || [ -z "${PCR1:-}" ] || [ -z "${PCR2:-}" ]; then
        echo "=== Step 3b-pre: Fetching PCRs from running enclave ===" >&2
        eval $(just proof-get-pcrs {{env}})
    fi

    echo "=== Step 3b: Approving PCR set ===" >&2
    just dry_run={{dry_run}} proof-approve-pcrs {{env}}

    # Verify rather than assume: re-measure the running enclave and confirm the allowlist
    # actually accepts it. A dry run approves nothing, so there is nothing to verify.
    if [ "{{dry_run}}" = "false" ]; then
        echo "=== Step 3c: Verifying the running enclave's PCR set ===" >&2
        just proof-verify-pcrs {{env}}
    fi

    echo "=== Deploy phases 0a-3c complete. ===" >&2
    echo "The game is registered but not activated; run 'just proof-activate-system {{env}}' after readiness checks." >&2
    echo "Next: register the enclave signing key (Phase 4) with 'just proof-register-key {{env}}'," >&2
    echo "      or run the worker with '--auto-register' so it self-registers on startup." >&2
