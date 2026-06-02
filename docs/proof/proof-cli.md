# `proof` CLI reference

The `proof` binary is the entry point for World Chain fault proof operations: witness generation,
SP1 zkVM proving, and AWS Nitro TEE attested proving.

```
proof <COMMAND>

Commands:
  hash-rollup-config   Print the rollup config hash used in proofs
  witness              Build and serialize a witness to a file
  sp1                  SP1 zkVM proving  [requires --features sp1]
  nitro                AWS Nitro TEE proving  [requires --features nitro]
```

## Building

```bash
# Witness generation only (no external prover deps)
cargo build -p proof

# With SP1 proving support
cargo build -p proof --features sp1

# With Nitro enclave support (Linux only — requires AF_VSOCK)
cargo build -p proof --features nitro

# Both
cargo build -p proof --features sp1,nitro
```

## Common environment variables

All RPC flags accept an environment variable fallback. The full set used across subcommands:

| Variable | Flag | Description |
|---|---|---|
| `L1_RPC_URL` | `--l1-rpc` | Ethereum L1 execution RPC |
| `L2_RPC_URL` | `--l2-rpc` | World Chain L2 execution RPC |
| `L1_BEACON_RPC_URL` | `--l1-beacon-rpc` | Ethereum L1 beacon API |
| `ROLLUP_CONFIG` | `--rollup-config` | Path to rollup config JSON |
| `ROLLUP_CONFIG_HASH` | `--rollup-config-hash` | Rollup config hash override |
| `L1_HEAD` | `--l1-head` | L1 head hash override |
| `NETWORK` | `--network` | `worldchain` (default) or `worldchain-sepolia` |
| `RANGE_ELF_PATH` | `--range-elf` | SP1 range program ELF path |
| `AGG_ELF_PATH` | `--agg-elf` | SP1 aggregation program ELF path |
| `SP1_PROVER` | `--prover` | SP1 backend: `cpu`, `network`, or `mock` |
| `SP1_PRIVATE_KEY` | — | Required for `--prover network` (sp1-sdk) |
| `ENCLAVE_CID` | `--cid` | vsock CID of the running Nitro enclave |
| `PCR0` / `PCR1` / `PCR2` | `--pcr0/1/2` | Expected Nitro PCR measurements |

A `.env` file in the working directory is loaded automatically.

---

## `hash-rollup-config`

Prints the 32-byte rollup config hash that the contracts and proof programs must agree on.

```
proof hash-rollup-config [--rollup-config <FILE> | --l2-rpc <URL>]
```

**Flags**

| Flag | Env | Description |
|---|---|---|
| `--rollup-config <FILE>` | `ROLLUP_CONFIG` | Read config from a local JSON file |
| `--l2-rpc <URL>` | `L2_RPC_URL` | Fetch config via `optimism_rollupConfig` |

One of the two is required; they are mutually exclusive.

**Example**

```bash
proof hash-rollup-config --rollup-config ./rollup.json
# 0x00821da4d0ba868e5eaa4fd2d6c486161b7bfc0ce3d0644ce79d3317f4f94c50

proof hash-rollup-config --l2-rpc https://rpc.world.org
```

---

## `witness`

Builds the Kona preimage witness for a block range and writes it to disk. Useful for inspecting
witness data or decoupling witness generation from proving.

```
proof witness [RPC flags] --output <FILE>
```

**Flags**

| Flag | Env | Default | Description |
|---|---|---|---|
| `--start-block <N>` | — | required | Exclusive lower bound of the proved range |
| `--end-block <N>` | — | required | Inclusive upper bound |
| `--l2-rpc <URL>` | `L2_RPC_URL` | required | World Chain L2 RPC |
| `--l1-rpc <URL>` | `L1_RPC_URL` | required | Ethereum L1 RPC |
| `--l1-beacon-rpc <URL>` | `L1_BEACON_RPC_URL` | required | L1 beacon API |
| `--rollup-config <FILE>` | `ROLLUP_CONFIG` | — | Rollup config JSON (mutually exclusive with `--rollup-config-hash`) |
| `--rollup-config-hash <HASH>` | `ROLLUP_CONFIG_HASH` | — | Precomputed hash (required when `--rollup-config` is not set) |
| `--l1-head <HASH>` | `L1_HEAD` | auto-resolved | Override the L1 checkpoint head |
| `--allow-unfinalized` | — | false | Allow proving blocks newer than the finalized L2 head |
| `--witness-timeout-seconds <N>` | — | 900 | Max seconds for Kona witness collection |
| `--network <NAME>` | `NETWORK` | `worldchain` | `worldchain` or `worldchain-sepolia` |
| `--output <FILE>` | — | required | Destination for the rkyv-serialized witness |

A `<stem>.metadata.json` file is written alongside the output with block metadata.

**Example**

```bash
proof witness \
  --start-block 10000000 \
  --end-block   10000100 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash 0x00821da4d0ba868e5... \
  --output ./witness.bin
```

---

## `sp1`

```
proof sp1 <COMMAND>

Commands:
  execute   Execute the SP1 range program locally (no ZK proof)
  prove     End-to-end range + aggregation proof from RPC
```

### `sp1 execute`

Runs the SP1 range program in non-proving execution mode against a pre-built witness file. Fast —
useful for checking the program terminates and inspecting cycle counts before committing to a full
proof.

```
proof sp1 execute --witness <FILE> --elf <FILE>
```

| Flag | Env | Description |
|---|---|---|
| `--witness <FILE>` | `WITNESS_PATH` | rkyv witness produced by `proof witness` |
| `--elf <FILE>` | `RANGE_ELF_PATH` | SP1 range ELF binary |

**Example**

```bash
proof sp1 execute \
  --witness ./witness.bin \
  --elf     ./elf/world-chain-range-ethereum
```

### `sp1 prove`

Generates range proofs for N equal sub-ranges and then aggregates them into a single proof,
entirely from RPC — no separate witness step needed.

```
proof sp1 prove [RPC flags] --range-elf <FILE> --agg-elf <FILE> [options]
```

**Flags**

| Flag | Env | Default | Description |
|---|---|---|---|
| `--start-block <N>` | — | required | |
| `--end-block <N>` | — | required | |
| `--l2-rpc <URL>` | `L2_RPC_URL` | required | |
| `--l1-rpc <URL>` | `L1_RPC_URL` | required | |
| `--l1-beacon-rpc <URL>` | `L1_BEACON_RPC_URL` | required | |
| `--rollup-config <FILE>` | `ROLLUP_CONFIG` | — | |
| `--rollup-config-hash <HASH>` | `ROLLUP_CONFIG_HASH` | — | |
| `--range-elf <FILE>` | `RANGE_ELF_PATH` | required | SP1 range ELF |
| `--agg-elf <FILE>` | `AGG_ELF_PATH` | required | SP1 aggregation ELF |
| `--ranges <N>` | — | `1` | Number of equal sub-ranges to prove in parallel |
| `--prover <NAME>` | `SP1_PROVER` | `cpu` | `cpu`, `network`, or `mock` |
| `--mode <NAME>` | — | `groth16` | Aggregation proof mode: `core`, `compressed`, `plonk`, `groth16` |
| `--prover-address <ADDR>` | — | zero address | On-chain attribution address |
| `--output <FILE>` | — | — | Write aggregation proof JSON to file |

**Prover backends**

- `cpu` — local CPU proving; needs 32–128 GB RAM.
- `network` — Succinct proving network; requires `SP1_PRIVATE_KEY` in the environment.
- `mock` — no real ZK, instant; for integration testing only; skips proof verification.

**Aggregation proof modes**

The `--mode` flag controls the proof system used for the final aggregation proof. Range proofs
are always proved in Compressed mode internally — this is required so the aggregation guest can
recursively verify them with `sp1_lib::verify::verify_sp1_proof`.

- `core` — proof size grows linearly with cycles. Not EVM-verifiable; use for debugging only.
- `compressed` — constant-size recursive proof; slower than core. Not EVM-verifiable.
- `plonk` — PLONK proof; ~300k gas to verify on-chain.
- `groth16` — **default**; Groth16 proof; ~100k gas to verify on-chain; use for production submissions.

**Example — mock proof (integration test)**

```bash
proof sp1 prove \
  --start-block 10000000 \
  --end-block   10000010 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash 0x00821da4d0ba868e5... \
  --range-elf ./elf/world-chain-range-ethereum \
  --agg-elf   ./elf/world-chain-aggregation \
  --prover    mock \
  --output    ./proof.json
```

**Example — network proof**

```bash
export SP1_PRIVATE_KEY=<your key>

proof sp1 prove \
  --start-block 10000000 \
  --end-block   10001000 \
  --ranges      4 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash 0x00821da4d0ba868e5... \
  --range-elf ./elf/world-chain-range-ethereum \
  --agg-elf   ./elf/world-chain-aggregation \
  --prover    network \
  --output    ./proof.json
```

---

## Running the Nitro enclave

**Requirements:** an EC2 instance type with Nitro Enclave support (e.g. `m5.xlarge`) and the
[AWS Nitro CLI](https://docs.aws.amazon.com/enclaves/latest/user/nitro-enclave-cli-install.html)
installed. The enclave binary and the `proof nitro prove` command must both run on the same
instance; vsock (AF_VSOCK) is Linux-only and does not cross machine boundaries.

### 1. Build the Docker image

Run from the repo root. The Dockerfile at `crates/proof/nitro/Dockerfile` compiles the enclave
binary inside the container.

```bash
docker build -t world-chain-nitro-enclave \
  -f crates/proof/nitro/Dockerfile .
```

### 2. Package as an EIF

```bash
nitro-cli build-enclave \
  --docker-uri world-chain-nitro-enclave:latest \
  --output-file world-chain-nitro-enclave.eif
```

`nitro-cli` prints the PCR measurements on success:

```
PCR0: <48-byte hex>   # EIF image hash
PCR1: <48-byte hex>   # kernel + bootstrap
PCR2: <48-byte hex>   # application
```

Save these — they are passed to `proof nitro prove` as `--pcr0/1/2`.

### 3. Run the enclave

```bash
nitro-cli run-enclave \
  --eif-path world-chain-nitro-enclave.eif \
  --memory 16384 \
  --cpu-count 4 \
  --enclave-cid 16
```

The enclave listens on vsock port **5005** by default. Override with `NITRO_VSOCK_PORT` if needed.

Check it started:

```bash
nitro-cli describe-enclaves
```

### 4. Prove from the host

```bash
cargo run -p proof --features nitro -- nitro prove \
  --start-block 29875200 \
  --end-block   29875800 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash $ROLLUP_CONFIG_HASH \
  --network worldchain-sepolia \
  --cid  16 \
  --pcr0 $PCR0 \
  --pcr1 $PCR1 \
  --pcr2 $PCR2 \
  --output ./nitro-artifact.json
```

### Stopping the enclave

```bash
nitro-cli terminate-enclave --enclave-id $(nitro-cli describe-enclaves | jq -r '.[0].EnclaveID')
```

---

## `nitro`

```
proof nitro <COMMAND>

Commands:
  prove   Generate witness and send to a Nitro enclave for attested proving
```

### `nitro prove`

Builds the witness locally and sends it to a running AWS Nitro enclave over vsock. The enclave
signs the result with an NSM attestation document; the host verifies the attestation and optionally
writes the artifact to disk.

**Requires:** Linux host with AF_VSOCK support; binary built with `--features nitro`.

```
proof nitro prove [RPC flags] [--cid <N>] [--pcr0/1/2 <HEX>] [--output <FILE>]
```

| Flag | Env | Default | Description |
|---|---|---|---|
| `--start-block <N>` | — | required | |
| `--end-block <N>` | — | required | |
| `--l2-rpc <URL>` | `L2_RPC_URL` | required | |
| `--l1-rpc <URL>` | `L1_RPC_URL` | required | |
| `--l1-beacon-rpc <URL>` | `L1_BEACON_RPC_URL` | required | |
| `--rollup-config <FILE>` | `ROLLUP_CONFIG` | — | |
| `--rollup-config-hash <HASH>` | `ROLLUP_CONFIG_HASH` | — | |
| `--cid <N>` | `ENCLAVE_CID` | `16` | vsock CID of the Nitro enclave |
| `--pcr0 <HEX>` | `PCR0` | — | Expected PCR0 (48-byte hex). Omit all three to skip image verification |
| `--pcr1 <HEX>` | `PCR1` | — | Expected PCR1 |
| `--pcr2 <HEX>` | `PCR2` | — | Expected PCR2 |
| `--output <FILE>` | — | — | Write JSON artifact (boot info + attestation doc hex) |

Providing any one of `--pcr0/1/2` without the other two is an error. Omitting all three skips PCR
verification — only appropriate in development.

**Example**

```bash
proof nitro prove \
  --start-block 10000000 \
  --end-block   10000100 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash 0x00821da4d0ba868e5... \
  --cid  16 \
  --pcr0 $PCR0 \
  --pcr1 $PCR1 \
  --pcr2 $PCR2 \
  --output ./nitro-artifact.json
```

The output JSON has the shape:

```json
{
  "bootInfo": { "l1Head": "0x...", "l2PreRoot": "0x...", "l2PostRoot": "0x...", "l2BlockNumber": 10000100, "rollupConfigHash": "0x..." },
  "attestationDoc": "0x<hex-encoded COSE_Sign1 bytes>"
}
```
