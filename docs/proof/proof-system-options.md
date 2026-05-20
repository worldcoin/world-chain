# World Chain Proof System Upgrade — Options

> **Status: WIP** — This document compares proof system options for upgrading World Chain's fault proof mechanism beyond vanilla Cannon. The current devnet rollout plan (OP Succinct Lite) is described in [devnet-succinct-fault-proof.md](./devnet-succinct-fault-proof.md).

## Background

World Chain currently runs a permissioned Cannon-based fault proof system (`game-type=1`) with `op-proposer` posting output roots every 20 minutes and `op-challenger` watching for disputes. The goal is to upgrade to a proof system that:

- Reduces the challenge window from 7 days to ~1 day
- Reduces bond requirements from hundreds of ETH to 5–15 ETH
- Eliminates the multi-round interactive bisection game
- Satisfies L2Beat Stage 1 requirements

Three approaches are evaluated below.

---

## Option 1: OP Succinct Lite (SP1 ZK Fault Proofs) ✅ Planned

### How it works

OP Succinct Lite replaces the interactive bisection game with a single ZK proof generated only when a dispute occurs. The chain runs optimistically by default — no proving cost on the happy path.

**Proof pipeline:**

```
L2 blocks (proposal range)
  │
  ├─ split into N sub-ranges (RANGE_SPLIT_COUNT)
  │
  ├─ parallel range proofs via Succinct Prover Network
  │    world-chain-range-ethereum ELF runs in SP1 zkVM (RISC-V)
  │    produces: compressed STARK proof per range
  │
  └─ aggregation proof
       world-chain-aggregation ELF verifies all range proofs
       produces: single Groth16/Plonk SNARK
         │
         └─ game.prove(agg_proof.bytes()) → L1 tx
              SP1_VERIFIER.verifyProof(AGGREGATION_VKEY, publicValues, proofBytes)
```

**Keys:**
- `aggregation_vkey` — SP1 verifying key, fingerprint of `world-chain-aggregation` ELF
- `range_vkey_commitment` — SP1 verifying key commitment for `world-chain-range-ethereum` ELF
- `rollup_config_hash` — SHA-256 of Kona `RollupConfig` + World-specific `tropo_time` + `strato_time`

All three must match between the deployed contract and the running proposer, or games are treated as foreign and will not be proven.

**World-specific changes vs upstream OP Succinct:**
- Replace ELF imports with `world-chain-proof-succinct-elfs`
- Replace `hash_rollup_config(...)` with World hash helper (adds `tropo_time`/`strato_time`)
- Replace ETH witness generation with World witness data carrying hardfork schedule

### Dispute flow

```
Proposer posts output root (no proof)
  │
  │  challenge window (~1 day)
  │
  ├─ No challenge → DEFENDER_WINS (no proof ever needed)
  │
  └─ Challenger calls challenge() + bond
       │
       Proposer requests ZK proof from Succinct Network (minutes–hours)
       Proposer calls prove(proofBytes)
         │
         ├─ proof valid → DEFENDER_WINS
         └─ proof missing/invalid by deadline → CHALLENGER_WINS
```

### Deployment

- New `game-type=42`
- New K8s apps: `world-chain-zk-proposer`, `world-chain-zk-challenger` in `crypto-apps`
- Existing Cannon challenger (`world-chain-challenger`) runs in parallel until ZK games are verified

### Properties

| Property | Value |
|---|---|
| Trust root | Cryptographic (SP1 zkVM math) |
| Challenge window | ~1 day |
| Bond size | 5–15 ETH |
| Proof generated | Only on dispute |
| On-chain verification gas | ~300k gas (Groth16 pairing check) |
| Finality | ~1 day (happy path), minutes after proof (disputed) |
| L2Beat Stage 1 eligible | ✅ Yes |
| AWS dependency | None (Succinct Prover Network) |
| Hardware dependency | None |

### Current blockers

- [ ] Finalize `tropo_time` and `strato_time` in `rollup.json`
- [ ] Deploy contracts with final `rollup_config_hash`, `aggregation_vkey`, `range_vkey_commitment`
- [ ] Build and publish custom World proposer image
- [ ] Add `world-chain-zk-proposer` and `world-chain-zk-challenger` to `crypto-apps`
- [ ] End-to-end verification on devnet

---

## Option 2: AWS Nitro Enclaves TEE

### How it works

Instead of a ZK proof, the L2 state transition function runs inside an AWS Nitro Enclave — a hardware-isolated VM partitioned from the parent EC2 instance. The Nitro Hypervisor signs an attestation document proving that a specific program (identified by PCR measurements) produced a specific output.

**Attestation pipeline:**

```
L2 blocks + previous state root
  │
  └─ load into Nitro Enclave (isolated VM, no network, no storage)
       │
       state transition function runs natively at full CPU speed
       computes: new_state_root
         │
         └─ request attestation from Nitro Hypervisor
              {
                PCR0: SHA384(enclave image),    ← program fingerprint
                PCR1: SHA384(kernel),
                PCR2: SHA384(application),
                user_data: new_state_root,       ← the output
              }
              signed with P-384 ECDSA by AWS Nitro PKI
                │
                └─ post attestation doc on-chain
                     L1 contract verifies:
                       - AWS PKI cert chain
                       - PCR0 matches expected image hash
                       - output_root extracted from user_data
```

**Setup on AWS:**

```bash
# 1. Launch Nitro-enabled EC2 instance
aws ec2 run-instances \
  --instance-type m5.xlarge \
  --enclave-options 'Enabled=true'

# 2. Install Nitro CLI
sudo amazon-linux-extras install aws-nitro-enclaves-cli

# 3. Build enclave image from Docker
nitro-cli build-enclave \
  --docker-uri world-chain-stf:latest \
  --output-file world-chain-stf.eif
# → outputs PCR0/PCR1/PCR2 measurements

# 4. Run enclave
nitro-cli run-enclave \
  --cpu-count 2 \
  --memory 2048 \
  --eif-path world-chain-stf.eif \
  --enclave-cid 16

# 5. Parent instance communicates via vsock only
# Enclave has: no external networking, no persistent storage
```

**On-chain verifier** must:
1. Verify COSE_Sign1 signature against hardcoded AWS Nitro root CA
2. Check PCR0 matches expected EIF hash (deployed at contract creation)
3. Extract `user_data` as the new state root

### Properties

| Property | Value |
|---|---|
| Trust root | AWS (owns attestation key + infrastructure) |
| Challenge window | Could be ~1 hour |
| Bond size | Low |
| Proof generated | Every block OR only on dispute |
| On-chain verification gas | ~21k gas (ECDSA signature check) |
| Finality | Could be minutes |
| L2Beat Stage 1 eligible | ⚠️ Unclear — depends on L2Beat's view of TEE trust |
| AWS dependency | **Hard dependency on AWS** |
| Hardware dependency | AWS Nitro Security Chip |

### Risks and limitations

- **AWS trust**: AWS owns both the attestation key and the infrastructure. If AWS is compromised or acts maliciously, proof integrity is lost. This is a centralised trust assumption fundamentally at odds with trustless rollup goals.
- **Not Intel SGX**: AWS Nitro Enclaves use hypervisor-based isolation, not CPU memory encryption. The isolation is equivalent to two separate EC2 instances — strong, but a different threat model than SGX.
- **No SGX on AWS**: AWS disables Intel SGX at the BIOS level on all EC2 instances. AWS Nitro is their proprietary alternative.
- **Certificate expiry**: AWS Nitro attestation certificates expire after 3 hours, requiring active rotation logic.
- **L2Beat classification**: TEE-based proofs are not currently recognised by L2Beat as equivalent to ZK proofs for Stage 1/2 classification.

---

## Option 3: Intel SGX TEE

### How it works

Intel SGX (Software Guard Extensions) is a CPU-level TEE. The state transition function runs inside an SGX _enclave_ — a private memory region that is hardware-encrypted and isolated even from the OS, hypervisor, and root users. Intel's Provisioning Certification Service (PCS) signs attestation quotes proving which program ran.

**Not available on AWS** — Intel SGX requires either:
- Azure DC-series instances (`Standard_DC4s_v3`, `Standard_DC8s_v3` etc.)
- IBM Cloud bare metal
- Physical Intel Skylake/Ice Lake servers with SGX enabled in BIOS

**Attestation pipeline:**

```
L2 blocks + previous state root
  │
  └─ load into SGX enclave (CPU-encrypted memory region)
       │
       state transition function runs natively
       computes: new_state_root
         │
         └─ generate SGX quote (DCAP attestation)
              {
                MRENCLAVE: SHA256(enclave code + data),  ← program fingerprint
                MRSIGNER:  SHA256(enclave signing key),
                report_data: new_state_root,              ← the output
              }
              signed by Intel Provisioning Certification Key (PCK)
                │
                └─ post quote on-chain
                     Automata DCAP contracts verify on-chain:
                       - PCK cert chain back to Intel root CA
                       - MRENCLAVE matches expected value
                       - report_data extracted as new state root
```

**Key difference from Nitro:** Trust is divided between Intel (attestation key) and the cloud/operator, rather than a single entity like AWS. The TCB (Trusted Computing Base) is also smaller — just the CPU, not a full hypervisor stack.

**On-chain verification:** [Automata Network](https://automata.network/) has deployed DCAP attestation contracts on Ethereum that EVM contracts can call to verify Intel SGX/TDX attestation quotes directly. A SNARK-compressed path reduces gas significantly.

### Properties

| Property | Value |
|---|---|
| Trust root | Intel (attestation key) + operator (hardware) |
| Challenge window | Could be ~1 hour |
| Bond size | Low |
| Proof generated | Every block OR only on dispute |
| On-chain verification gas | ~300k+ gas (DCAP verification) or ~30k (SNARK-compressed via Automata) |
| Finality | Could be minutes |
| L2Beat Stage 1 eligible | ⚠️ Unclear — same TEE trust concerns as Nitro |
| AWS dependency | None (but requires Azure/IBM/bare metal) |
| Hardware dependency | Intel CPU with SGX enabled |

### Risks and limitations

- **Intel trust**: Intel owns the PCK signing key infrastructure. A compromised Intel PCS undermines all attestation proofs. Side-channel attacks (Spectre, Plundervolt, SGAxe, Foreshadow) have repeatedly broken SGX enclaves.
- **Not on AWS**: Requires migrating sequencer/proposer infra to Azure or bare metal — significant operational change for TFH.
- **EPC memory limits**: SGX1 limits enclave memory to ~256MB. Full L2 state transition functions may exceed this. SGX2 improves this but requires newer hardware.
- **Maintenance burden**: SGX enclaves require regular TCB recovery updates as Intel patches microcode.

---

## Comparison Summary

| | OP Succinct Lite (SP1) | AWS Nitro TEE | Intel SGX TEE |
|---|---|---|---|
| **Trust root** | Math only | AWS | Intel |
| **Available on AWS** | ✅ Yes | ✅ Yes | ❌ No |
| **Challenge window** | ~1 day | ~1 hour | ~1 hour |
| **Happy path proving cost** | $0 | $0 | $0 |
| **Disputed proving cost** | Succinct prover fee | ~$0 | ~$0 |
| **On-chain gas** | ~300k (Groth16) | ~21k (ECDSA) | ~30k–300k (DCAP) |
| **Proof speed** | Minutes–hours | Seconds | Seconds |
| **L2Beat Stage 1** | ✅ Yes | ⚠️ Unclear | ⚠️ Unclear |
| **Trustless** | ✅ Yes | ❌ No (trust AWS) | ❌ No (trust Intel) |
| **Known exploits** | None on production | Hypervisor attacks | Side-channel attacks |
| **Infra change needed** | Minimal (same AWS) | Minimal (same AWS) | Large (need Azure/bare metal) |
| **World-specific code** | ELF swap + config hash | STF in Docker | STF in enclave |
| **Status** | ✅ Planned (devnet WIP) | 🔬 Research only | 🔬 Research only |

## Recommendation

**OP Succinct Lite (SP1)** is the recommended path and is already in active development. It is the only option that provides cryptographic (trustless) finality guarantees, satisfies L2Beat Stage 1 requirements, and requires no hardware vendor trust assumptions.

TEE-based approaches (AWS Nitro, Intel SGX) offer faster finality and lower on-chain gas costs but introduce hardware vendor trust that is incompatible with World Chain's trustlessness goals for rollup security. They may be worth revisiting as complementary mechanisms (e.g. sequencer fairness, MEV protection) rather than primary proof systems.

## References

- [World Chain devnet Succinct fault proof rollout](./devnet-succinct-fault-proof.md)
- [OP Succinct Lite architecture](https://succinctlabs.github.io/op-succinct/fault_proofs/fault_proof_architecture.html)
- [OP Succinct proposer](https://succinctlabs.github.io/op-succinct/fault_proofs/proposer.html)
- [AWS Nitro Enclaves attestation](https://docs.aws.amazon.com/enclaves/latest/user/set-up-attestation.html)
- [Intel SGX overview](https://www.intel.com/content/www/us/en/developer/tools/software-guard-extensions/overview.html)
- [Automata DCAP on-chain verification](https://automata.network/)
- [OP Kailua (RISC Zero alternative)](https://risczero.com/op-kailua)
