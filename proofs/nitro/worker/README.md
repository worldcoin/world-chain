# `world-chain-nitro-worker`

The `nitro-worker` is the AWS Nitro TEE proving worker of the World Chain
defender stack. It is the TEE analogue of the
[`sp1-worker`](../../sp1-worker): it leases [`ProofBackend::Nitro`] jobs from
the `prover-service`, builds the range witness over RPC, hands it to a running
Nitro Enclave for attested derivation, and submits the resulting attestation
document + signature back to the `prover-service`.

```text
PROVER-SERVICE --getNextProof(Nitro)--> NITRO-WORKER --witness--> NITRO ENCLAVE
       ^                                                                 |
       +------------------------ submitProof(attestation, signature) <---+
```

The worker only compiles on Linux because it relies on `AF_VSOCK` to talk to
the enclave.

## CLI / environment options

All flags can also be supplied via the environment variable named in the
`env =` attribute. `dotenvy` is loaded at startup, so a local `.env` file
works too (see [`.env.example`](.env.example)).

| Flag                          | Env                       | Default              | Description |
| ---                           | ---                       | ---                  | --- |
| `--prover-service-url`        | `PROVER_SERVICE_URL`      | _required_           | prover-service JSON-RPC URL. |
| `--l2-rpc`                    | `L2_RPC_URL`              | _required_           | World Chain L2 execution RPC URL. |
| `--l1-rpc`                    | `L1_RPC_URL`              | _required_           | Ethereum L1 execution RPC URL. |
| `--l1-beacon-rpc`             | `L1_BEACON_RPC_URL`       | _required_           | Ethereum L1 beacon API URL. |
| `--network`                   | `NETWORK`                 | `worldchain`         | `worldchain` or `worldchain-sepolia`. |
| `--rollup-config`             | `ROLLUP_CONFIG`           | built-in for network | Override rollup config JSON. |
| `--rollup-config-hash`        | `ROLLUP_CONFIG_HASH`      | derived              | Override rollup config hash. |
| `--block-interval`            | `BLOCK_INTERVAL`          | _required_           | L2 blocks between a proposal's parent and its claimed block (proof system `blockInterval`). |
| `--enclave-cid`               | `ENCLAVE_CID`             | `16`                 | vsock CID of the Nitro Enclave. |
| `--enclave-port`              | `ENCLAVE_PORT`            | `5005`               | vsock port the enclave listens on. |
| `--pcr0` / `--pcr1` / `--pcr2`| `PCR0` / `PCR1` / `PCR2`  | _required (prod)_    | Expected PCR measurements (48-byte hex). |
| `--dev`                       | `NITRO_WORKER_DEV`        | `false`              | Permit launching without PCRs by defaulting to placeholder zeros. **Devnet only.** |
| `--poll-interval-seconds`     | `POLL_INTERVAL_SECONDS`   | `10`                 | Sleep between job-queue polls. |
| `--witness-timeout-seconds`   | —                         | `900`                | Cap on a single Kona witness build. |
| `--max-concurrent-jobs`       | —                         | `1`                  | Number of jobs proved in parallel. |

## Devnet mode

The native World Chain devnet (`just devnet up`) ships an in-process Nitro
worker that is opt-in via the `DEVNET_NITRO_WORKER_ENDPOINT` environment
variable. Setting it to `cid` (uses the default vsock port `5005`) or
`cid:port` starts the worker pointed at that enclave:

```bash
# Run the native devnet with the in-process Nitro worker enabled.
DEVNET_NITRO_WORKER_ENDPOINT=16:5005 just devnet up
```

Devnet always runs the worker in dev mode:

- `expected_pcrs = ExpectedPcrs::PLACEHOLDER` (all zeros), which tells the
  host-side verifier to skip attestation and signature checks. **Never use
  this mode in production.**
- The worker only starts when the WIP-1006 proof-system contracts are also
  deployed (the default for `ha-sequencer`). Use `--no-proof-system` to
  intentionally run the devnet without the proving loop.
- The in-process defender `prover-service` is started on demand and shared
  with the SP1 worker (when both are enabled).

Run the binary directly against the devnet for ad-hoc experimentation:

```bash
PROVER_SERVICE_URL=http://127.0.0.1:8551 \
L1_RPC_URL=http://127.0.0.1:8545 \
L1_BEACON_RPC_URL=http://127.0.0.1:8545 \
L2_RPC_URL=http://127.0.0.1:8546 \
BLOCK_INTERVAL=10 \
ENCLAVE_CID=16 \
NITRO_WORKER_DEV=1 \
cargo run -p world-chain-nitro-worker --bin nitro-worker
```

(Devnet ports are dynamic by default; the actual URLs are printed at startup
and in `target/devnet/endpoints.json`.)

## Production mode

For staging/production runs, supply **all three** PCRs measured from the
registered enclave image:

```bash
PROVER_SERVICE_URL=https://prover-service.example \
L1_RPC_URL=https://l1.example \
L1_BEACON_RPC_URL=https://beacon.example \
L2_RPC_URL=https://l2.example \
BLOCK_INTERVAL=10 \
PCR0=... PCR1=... PCR2=... \
nitro-worker
```

Omitting PCRs without `--dev` is a hard error so the worker never silently
runs with attestation verification disabled.

## Container image

The `Dockerfile.proof` at the repo root can build the worker:

```bash
docker buildx build \
  --build-arg PROVER_PACKAGE=world-chain-nitro-worker \
  --build-arg PROVER_BIN=nitro-worker \
  --build-arg FEATURES= \
  -f Dockerfile.proof \
  -t world-chain-nitro-worker:latest .
```

The enclave image itself is built from
[`proofs/nitro/Dockerfile`](../Dockerfile) and converted to an EIF with
[`scripts/build-eif.sh`](../../../scripts/build-eif.sh).

[`ProofBackend::Nitro`]: ../../prover-service/src/types.rs
