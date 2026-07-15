# World Chain prover CLI reference

The host-side prover entry points are split by backend. Both binaries share witness generation and
rollup-config hashing, while backend-specific commands live at the top level of each binary.

```
world-chain-prover-sp1 <COMMAND>

Commands:
  hash-rollup-config   Print the rollup config hash used in proofs
  witness              Build and serialize a witness to a file
  execute              Estimate SP1 range PGUs without generating a proof
  prove                Generate range + aggregation proofs
  vkeys                Compute the SP1 verification keys
```

```
world-chain-prover-nitro <COMMAND>

Commands:
  hash-rollup-config   Print the rollup config hash used in proofs
  witness              Build and serialize a witness to a file
  prove                Generate witness and send it to a Nitro enclave
  get-attestation      Fetch a bare attestation from a running enclave
```

## Building

```bash
# SP1 prover
cargo build -p world-chain-prover-sp1

# Nitro enclave prover (Linux only, requires AF_VSOCK)
cargo build -p world-chain-prover-nitro

# Shared library only
cargo build -p world-chain-prover --lib
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
| `SP1_PROVER` | `--prover` | SP1 backend: `cpu`, `network`, or `mock` |
| `SP1_PRIVATE_KEY` | `--sp1-private-key` | Required for `--prover network` (sp1-sdk) |
| `ENCLAVE_CID` | `--cid` | vsock CID of the running Nitro enclave |
| `ENCLAVE_PORT` | `--port` | vsock port of the running Nitro enclave |
| `PCR0` / `PCR1` / `PCR2` | `--pcr0/1/2` | Expected Nitro PCR measurements |

A `.env` file in the working directory is loaded automatically.

---

## `hash-rollup-config`

Prints the 32-byte rollup config hash that the contracts and proof programs must agree on.

```
world-chain-prover-sp1 hash-rollup-config [--rollup-config <FILE> | --l2-rpc <URL>]
world-chain-prover-nitro hash-rollup-config [--rollup-config <FILE> | --l2-rpc <URL>]
```

**Flags**

| Flag | Env | Description |
|---|---|---|
| `--rollup-config <FILE>` | `ROLLUP_CONFIG` | Read config from a local JSON file |
| `--l2-rpc <URL>` | `L2_RPC_URL` | Fetch config via `optimism_rollupConfig` |

One of the two is required; they are mutually exclusive.

**Example**

```bash
world-chain-prover-sp1 hash-rollup-config --rollup-config ./rollup.json
# 0x00821da4d0ba868e5eaa4fd2d6c486161b7bfc0ce3d0644ce79d3317f4f94c50

world-chain-prover-sp1 hash-rollup-config --l2-rpc https://rpc.world.org
```

---

## `witness`

Builds the Kona preimage witness for a block range and writes it to disk. Useful for inspecting
witness data or decoupling witness generation from proving.

```
world-chain-prover-sp1 witness [RPC flags] --output <FILE>
world-chain-prover-nitro witness [RPC flags] --output <FILE>
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
world-chain-prover-sp1 witness \
  --start-block 10000000 \
  --end-block   10000100 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash 0x00821da4d0ba868e5... \
  --output ./witness.bin
```

---

## `world-chain-prover-sp1`

```
world-chain-prover-sp1 <COMMAND>

Commands:
  hash-rollup-config   Print the rollup config hash used in proofs
  witness              Build and serialize a witness to a file
  execute              Estimate SP1 range PGUs without generating a proof
  prove                End-to-end range + aggregation proof from RPC
  vkeys                Compute the on-chain verification keys
```

### `execute`

Builds a witness for the requested block range in memory, then executes the embedded production
range ELF locally with SP1 gas calculation enabled. It does not generate or submit a proof and does
not require an SP1 private key.

```
world-chain-prover-sp1 execute [RPC flags]
```

| Flag | Env | Default | Description |
|---|---|---|---|
| `--start-block <N>` | — | required | Exclusive lower bound of the executed range |
| `--end-block <N>` | — | required | Inclusive upper bound |
| `--l2-rpc <URL>` | `L2_RPC_URL` | required | World Chain L2 execution RPC |
| `--l1-rpc <URL>` | `L1_RPC_URL` | required | Ethereum L1 execution RPC |
| `--l1-beacon-rpc <URL>` | `L1_BEACON_RPC_URL` | required | Ethereum L1 beacon API |
| `--rollup-config <FILE>` | `ROLLUP_CONFIG` | — | Rollup config JSON |
| `--rollup-config-hash <HASH>` | `ROLLUP_CONFIG_HASH` | — | Required when `--rollup-config` is omitted |
| `--l1-head <HASH>` | `L1_HEAD` | auto-resolved | Override the L1 checkpoint head |
| `--allow-unfinalized` | — | false | Allow executing blocks newer than the finalized L2 head |
| `--witness-timeout-seconds <N>` | — | 900 | Maximum seconds for witness collection |
| `--network <NAME>` | `NETWORK` | `worldchain` | `worldchain` or `worldchain-sepolia` |

**Example**

```bash
world-chain-prover-sp1 execute \
  --start-block 10000000 \
  --end-block   10000010 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash $ROLLUP_CONFIG_HASH
```

The output includes the normalized PGU estimate used by SP1 Network as the range request's gas
limit, plus total cycles, syscalls, and public values. This covers only the range request. The
aggregation request, network base fee, and dynamic price per PGU are separate costs.

### `prove`

Generates a compressed range proof and then aggregates it into a final proof, entirely from RPC.
No separate witness step is needed.

```
world-chain-prover-sp1 prove [RPC flags] [options]
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
| `--ranges <N>` | — | `1` | Number of sub-ranges; currently must be `1` |
| `--prover <NAME>` | `SP1_PROVER` | `cpu` | `cpu`, `network`, or `mock` |
| `--mode <NAME>` | — | `groth16` | Aggregation proof mode: `core`, `compressed`, `plonk`, `groth16` |
| `--prover-address <ADDR>` | — | zero address | On-chain attribution address |
| `--output <FILE>` | — | — | Write aggregation proof JSON to file |

The range and aggregation ELFs are embedded into the SP1 prover binary at compile time —
there are no `--range-elf` / `--agg-elf` flags. See [`elf-management.md`](./elf-management.md).

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
world-chain-prover-sp1 prove \
  --start-block 10000000 \
  --end-block   10000010 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash 0x00821da4d0ba868e5... \
  --prover    mock \
  --output    ./proof.json
```

**Example — network proof**

```bash
export SP1_PRIVATE_KEY=<your key>

world-chain-prover-sp1 prove \
  --start-block 10000000 \
  --end-block   10001000 \
  --l2-rpc      $L2_RPC_URL \
  --l1-rpc      $L1_RPC_URL \
  --l1-beacon-rpc $L1_BEACON_RPC_URL \
  --rollup-config-hash 0x00821da4d0ba868e5... \
  --prover    network \
  --output    ./proof.json
```

### `vkeys`

Computes the on-chain verification keys for the (embedded) range and aggregation ELFs: the
range vkey commitment (`multiBlockVKey` committed by the aggregation guest) and the
aggregation vkey registered with the SP1 verifier. Runs SP1 setup locally — no proving, no RPC,
no arguments.

```
world-chain-prover-sp1 vkeys [--output <FILE>]
```

| Flag | Env | Default | Description |
|---|---|---|---|
| `--output <FILE>` | — | stdout | Write the JSON here instead of stdout |

**Example**

```bash
just proof-vkeys
```

```json
{
  "range_vkey_commitment": "0x…",
  "aggregation_vkey": "0x…",
  "elfs": {
    "world-chain-range-ethereum": { "sha256": "…" },
    "world-chain-aggregation":    { "sha256": "…" }
  }
}
```

---

## Running the Nitro enclave

**Requirements:** an EC2 instance type with Nitro Enclave support (e.g. `m5.xlarge`) and the
[AWS Nitro CLI](https://docs.aws.amazon.com/enclaves/latest/user/nitro-enclave-cli-install.html)
installed. The enclave binary and the `world-chain-prover-nitro prove` command must both run on
the same instance; vsock (AF_VSOCK) is Linux-only and does not cross machine boundaries.

### 1. Build the Docker image

Run from the repo root. The Dockerfile at `proofs/nitro/Dockerfile` compiles the enclave
binary inside the container.

```bash
docker build -t world-chain-nitro-enclave \
  -f proofs/nitro/Dockerfile .
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

Save these — they are passed to `world-chain-prover-nitro prove` as `--pcr0/1/2`.

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
cargo run -p world-chain-prover-nitro -- prove \
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

## `world-chain-prover-nitro`

```
world-chain-prover-nitro <COMMAND>

Commands:
  hash-rollup-config   Print the rollup config hash used in proofs
  witness              Build and serialize a witness to a file
  prove                Generate witness and send to a Nitro enclave for attested proving
  get-attestation      Fetch a bare attestation from a running enclave
```

### `prove`

Builds the witness locally and sends it to a running AWS Nitro enclave over vsock. The enclave
signs the result with an NSM attestation document; the host verifies the attestation and optionally
writes the artifact to disk.

**Requires:** Linux host with AF_VSOCK support.

```
world-chain-prover-nitro prove [RPC flags] [--cid <N>] [--pcr0/1/2 <HEX>] [--output <FILE>]
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
| `--port <N>` | `ENCLAVE_PORT` | `5005` | vsock port the enclave listens on |
| `--pcr0 <HEX>` | `PCR0` | — | Expected PCR0 (48-byte hex) — required |
| `--pcr1 <HEX>` | `PCR1` | — | Expected PCR1 — required |
| `--pcr2 <HEX>` | `PCR2` | — | Expected PCR2 — required |
| `--output <FILE>` | — | — | Write JSON artifact (boot info + attestation doc hex) |

All three of `--pcr0`, `--pcr1`, and `--pcr2` must be provided; providing only a subset is
an error. PCR values are the hex-encoded 48-byte enclave measurements that identify the
exact EIF image running in the Nitro enclave.

**Example**

```bash
world-chain-prover-nitro prove \
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

### `get-attestation`

Fetches a bare NSM attestation document from a running Nitro enclave. No proof is generated
and no RPC endpoints are required — the enclave simply calls its NSM device and returns the
raw `COSE_Sign1` bytes. Connects to CID 16 on the default vsock port (5005) and prints the
hex-encoded attestation to stdout.

This is primarily used for the **CertManager pre-warm** workflow (see below).

**Requires:** Linux host with AF_VSOCK support and a running Nitro enclave.

```
world-chain-prover-nitro get-attestation
```

This subcommand takes no flags.

**Example**

```bash
cargo run -p world-chain-prover-nitro -- get-attestation > /tmp/attestation.hex
```

---

## CertManager pre-warm workflow

When deploying the Nitro proof system from scratch, the `CertManager` contract on L1 must
be pre-warmed with the AWS Nitro CA certificate chain **before** any `registerKey` call can
succeed. This creates a chicken-and-egg problem: to register an enclave's key you need the
CA chain on-chain, and to get the CA chain you need a real attestation document from a
running enclave.

The `get-attestation` subcommand solves this by providing a lightweight way to obtain an
attestation document without running a full proof.

### Step 1 — Start the enclave

Build and run the Nitro enclave as described in the [Running the Nitro enclave](#running-the-nitro-enclave)
section above.

### Step 2 — Fetch the attestation document

```bash
cargo run -p world-chain-prover-nitro -- get-attestation > /tmp/attestation.hex
```

The file contains the hex-encoded `COSE_Sign1` attestation bytes.

### Step 3 — Pre-warm CertManager

Pipe the attestation into the `hinted_attestation_calls.js` tool to extract the CA chain
and submit it to `CertManager`:

```bash
cat /tmp/attestation.hex | xargs -I{} node tools/hinted_attestation_calls.js prepare \
  --attestation {} \
  --cert-manager 0x<CertManager address>
```

### Step 4 — Approve PCR set (Phase 3)

Once the CA chain is registered, complete the setup by approving the enclave's PCR
measurements:

```bash
node tools/hinted_attestation_calls.js approvePCRSet \
  --pcr0 $PCR0 \
  --pcr1 $PCR1 \
  --pcr2 $PCR2 \
  --cert-manager 0x<CertManager address>
```

After this, `registerKey` calls from the enclave will succeed.
