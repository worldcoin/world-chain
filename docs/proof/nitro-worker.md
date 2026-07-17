# Nitro Worker: How It Works

## Overview

The nitro worker is World Chain's TEE-based proving backend. Instead of generating a
zero-knowledge proof (which is computationally expensive and slow), it re-executes the
L2 state transition inside an **AWS Nitro Enclave** — a hardware-isolated VM whose
identity and outputs are cryptographically attested by Amazon's Nitro Security Module
(NSM). The enclave produces two artifacts: an **NSM attestation document** (proving
_what code ran_ and _what it computed_) and a **secp256k1 signature** (cheaply
verifiable on-chain). Together these let the on-chain contracts accept a state root
without a ZK proof.

The system spans two binaries, three Kubernetes containers, and four smart contracts.
This document explains every layer.

> **Comparison with Base:** World Chain's Nitro implementation is directly inspired by
> Base's [`op-enclave`](https://github.com/base/op-enclave) project and reuses Base's
> [`nitro-validator`](https://github.com/base/nitro-validator) Solidity library. The
> core on-chain attestation verification (COSE_Sign1 parsing, hinted P-384, cert chain,
> `ecrecover`) is shared. The key differences are: (1) World Chain integrates as an OP
> Stack dispute game lane, while Base's op-enclave replaces the proposer outright; (2)
> World Chain checks PCR0+PCR1+PCR2 triples, Base checks only PCR0; (3) World Chain
> has a 3-state key lifecycle with permanent revocation, Base uses a simple
> add/delete map. Details are compared throughout this document.

---

## Architecture

### The 2-Container Pod

The `nitro-worker` Kubernetes pod contains two containers:

```
┌─────────────────────────────────────────────────────────────────────────┐
│  EC2 Node (Nitro-capable)                                               │
│                                                                         │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │  nitro-worker Pod                                                │  │
│  │                                                                  │  │
│  │  ┌──────────────────────────┐  ┌──────────────────────────────┐ │  │
│  │  │  nitro-worker (main)     │  │  enclave-launcher (sidecar)  │ │  │
│  │  │                          │  │                              │ │  │
│  │  │  polls prover-service    │  │  runs nitro-cli run-enclave  │ │  │
│  │  │  builds witnesses        │  │  writes CID ─────────────────┼─┼► │
│  │  │  sends to enclave        │  │  to /run/nitro-shared/       │ │  │
│  │  │  reads CID ◄─────────────┼──┼──────────────────────────── │ │  │
│  │  └────────────┬─────────────┘  └──────────────────────────────┘ │  │
│  └───────────────┼────────────────────────────────────────────────── ┘  │
│                  │ vsock (CID from /run/nitro-shared/enclave-cid)       │
│                  ▼                                                      │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │  AWS Nitro Enclave  (world-chain-nitro-enclave)                   │  │
│  │                                                                   │  │
│  │  - Listens on vsock port 5005                                     │  │
│  │  - Re-executes Kona derivation pipeline                           │  │
│  │  - Signs output with ephemeral secp256k1 key                      │  │
│  │  - Requests NSM attestation embedding key + user_data             │  │
│  │  - Zero network access (vsock only)                               │  │
│  └───────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

### On-Chain Contract Stack

```
NitroProofVerifier           ← World Chain addition: dispute game lane hook
  │  ecrecover signature → check key is registered
  ▼
NitroEnclaveKeyRegistry      ← World Chain addition: 3-state key lifecycle
  │  registerKey() / revokeKey() / isKeyRegistered()
  │  key lifecycle: Unknown → Active → Revoked
  ▼
NitroAttestationVerifier     ← World Chain addition: PCR triple allowlist + timestamps
  │  parse COSE_Sign1, verify P-384 sig, check PCR0+PCR1+PCR2
  ▼
NitroValidator               ← Base's library (base/nitro-validator)
  │  CBOR parse, hinted P-384 ECDSA, cert chain walk
  ▼
CertManager (ICertManager)   ← Base's library
  │  X.509 certificate chain validation + caching
  │  pinned to AWS Nitro Root CA
  ▼
AWS Nitro Root CA (hardcoded)
```

> **Base comparison:** Base's `SystemConfigGlobal` sits at approximately the
> `NitroAttestationVerifier` + `NitroEnclaveKeyRegistry` layer — it validates the
> attestation and stores valid signer addresses. World Chain adds `NitroProofVerifier`
> on top to integrate with the OP Stack dispute game interface
> (`IWorldChainProofVerifier`). Base doesn't have an equivalent because their system
> replaces the proposer rather than plugging into the dispute game.

---

## Key Concepts

### vsock

**What it is:** A hypervisor-backed socket family (`AF_VSOCK`) that connects a Nitro
Enclave to its parent EC2 instance. Each side is identified by a Context ID (CID) and
a port number.

**Why it matters:** Nitro Enclaves have _no_ network interfaces — no TCP, no UDP, no
DNS. vsock is the **only** communication channel. The enclave listens on
`VMADDR_CID_ANY` (accepts any CID) on port 5005. The host connects to the enclave's
assigned CID, which `nitro-cli run-enclave` prints and the `enclave-launcher` sidecar
writes to a shared file.

There is currently no vsock-proxy in the deployment — the enclave makes no outbound
AWS API calls. If KMS key persistence is implemented in the future, a vsock-proxy
will be needed to bridge the enclave's vsock traffic to external AWS endpoints.
See [Key Persistence](#key-persistence) and the [KMS improvement proposal](#future-improvement-kms-based-key-persistence).

### NSM (Nitro Security Module)

**What it is:** A hardware device inside every Nitro Enclave that provides:
- Cryptographic attestation (signed statements about the enclave's identity and state)
- Hardware-backed random number generation

**Why it matters:** The NSM is the root of trust. When asked for an attestation, it
returns a COSE_Sign1-signed document containing PCR measurements (what code is
running), optional `user_data` (what the enclave claims it computed), an optional
`nonce` (for freshness), and an optional `public_key` field. The attestation is signed
by a certificate chain rooted at the AWS Nitro Root CA, so anyone can verify it was
produced by a genuine Nitro Enclave.

The enclave interacts with NSM via an ioctl file descriptor obtained from `nsm_init()`.

### COSE_Sign1

**What it is:** A CBOR-encoded signed message format (RFC 8152). It wraps a payload
and a single signature.

**Why it matters:** NSM attestation documents are COSE_Sign1 structures. The payload
is a CBOR map containing PCR measurements, the module ID, a timestamp, certificate
chain, and optional user-supplied fields (`user_data`, `nonce`, `public_key`). The
signature uses P-384 ECDSA, signed by the leaf certificate in the embedded chain.

### PCR Measurements

**What they are:** Platform Configuration Registers — SHA-384 hashes (48 bytes each)
that fingerprint the enclave image:

| PCR | Contents |
|-----|----------|
| PCR0 | Hash of the entire EIF (kernel + ramdisk + bootstrap) — the **primary identity** |
| PCR1 | Hash of kernel + bootstrap only |
| PCR2 | Hash of application code only |

**Why they matter:** PCRs let verifiers assert _which code_ produced the attestation.
The on-chain `NitroAttestationVerifier` maintains an allowlist of approved PCR triples.
If you rebuild the enclave binary, PCR values change, and you must approve the new set
before keys from that enclave can be registered.

### Ephemeral Signing Key

**What it is:** At startup, the enclave requests 32 bytes of hardware-backed entropy
from the NSM (`NsmRequest::GetRandom`) and derives a secp256k1 signing key. This key
is stored in a `OnceLock<SigningKey>` — it lives only in enclave memory and is
generated fresh on every enclave boot.

**Why it matters:** NSM attestation uses P-384, which is expensive to verify on the
EVM. The ephemeral secp256k1 key lets the enclave produce signatures that are
EVM-native (`ecrecover` costs only 3,000 gas). The NSM attestation certifies the
public key (embedded in the `public_key` field), binding the cheap signature to the
hardware attestation.

### CertManager

**What it is:** An on-chain contract that validates X.509 certificate chains. It caches
validated certificates so repeat verifications are cheaper.

**Why it matters:** NSM attestation documents include a certificate chain from the
signing certificate up to the AWS Nitro Root CA. `NitroAttestationVerifier` delegates
chain validation to `CertManager`. Because certificate validation is expensive
(~63M gas for the full chain), the certs must be **pre-warmed** — validated and cached
once — before any `registerKey` call can succeed.


---

## Key Persistence

The secp256k1 signing key is the enclave's long-term identity — it is registered
on-chain and used to sign every range proof. How this key survives (or doesn't
survive) enclave restarts has significant operational implications.

### Current behaviour: ephemeral, re-register on restart

The signing key is generated fresh from NSM entropy (`NsmRequest::GetRandom`) on
every enclave boot and lives only in enclave memory. When the enclave stops the key
is gone. The new key generated on next boot must be re-registered on-chain before
it can be used to verify proofs, causing a brief liveness gap.

**On-chain impact:** the old key stays `Active` in `NitroEnclaveKeyRegistry` (it
was never revoked), but it cannot sign anything — the enclave that held its private
key no longer exists. The new key needs a fresh `registerKey` call.

### How Base solves it: RSA-sealing (for reference)

Base's [`op-enclave`](https://github.com/base/op-enclave) avoids re-registration by
sealing the secp256k1 signing key with RSA encryption so the proposer can hand it
back to a restarted enclave. The enclave holds two keys at startup:

- A **secp256k1 signing key** — for signing state root proposals (registered on-chain)
- An **RSA-4096 decryption key** — generated from NSM entropy, used only as a
  one-time key-transport wrapper

The persistence flow on enclave restart:

```
  Proposer                      New enclave (just started)
     │                                  │
     │─ DecryptionAttestation() ───────►│  NSM signs attestation with
     │◄─ COSE_Sign1 doc ───────────────-│  new RSA public key in public_key field
     │
     │─ EncryptedSignerKey(attestation) ─► Old enclave (still running)
     │                                       1. Verifies attestation (PCR0 check)
     │                                       2. Extracts RSA pubkey from doc
     │                                       3. Encrypts secp256k1 key bytes with it
     │◄─ RSA-encrypted ciphertext ──────────
     │
     │─ SetSignerKey(ciphertext) ────────► New enclave
     │                                       Decrypts with its RSA private key
     │                                       Now has the same secp256k1 key
```

The ciphertext lives in RAM on the proposer process — not in a database, not in KMS.
The proposer only hands the key to an enclave whose NSM attestation shows PCR0 matches
the approved image. The RSA-sealing approach avoids re-registration but couples the
restart to the proposer being online to orchestrate the handoff.

### Proposed improvement: KMS-based persistence

A cleaner alternative using AWS KMS attestation-based key policies is described in
[Future Improvement: KMS-Based Key Persistence](#future-improvement-kms-based-key-persistence)
below. That approach lets the enclave unseal its own key autonomously on restart,
without the proposer being involved.

---
## End-to-End Proving Flow

Here is the full journey from "job available" to "proof accepted":

1. **Job leased.** The `nitro-worker` polls the `prover-service` API for pending jobs
   with `ProofBackend::Nitro`. It receives a job containing an L2 block range to prove.

2. **Witness built.** The worker calls `build_range_input()`, which queries L1/L2 RPC
   endpoints to gather all the data the Kona derivation pipeline needs: L1 blocks, L2
   blocks, receipts, transactions, and state data. This produces a
   `WorldRangeWitnessData` struct.

3. **Witness serialized.** The witness is serialized to bytes using `rkyv`
   (zero-copy serialization).

4. **Nonce generated.** The worker generates 32 random bytes from `/dev/urandom`.
   This nonce will be embedded in the NSM attestation to prevent replay attacks.

5. **Request sent to enclave.** The worker sends an `EnclaveRequest::Range` message
   over vsock, containing the rkyv-serialized witness, expected public values, and the
   nonce. The wire protocol is: 4-byte big-endian length prefix, then CBOR-encoded
   payload.

6. **Enclave deserializes witness.** Inside the enclave, the rkyv bytes are
   deserialized back into `WorldRangeWitnessData`.

7. **State transition executed.** The enclave runs the full Kona derivation pipeline
   (`run_full_range_program`), re-deriving the L2 state from L1 data. This produces
   `TransitionPublicValues` containing the pre/post roots, pre/post block numbers, and
   rollup config hash.

8. **Optional validation.** If `expected_transition_public_values` was provided, the enclave
   checks that the computed `TransitionPublicValues` matches.

9. **Signing commitment computed.** The enclave computes
   `keccak256(l2_post_root || l2_block_number_be || rollup_config_hash)`.

10. **Signature produced.** The enclave signs the commitment with its ephemeral
    secp256k1 key, applying EIP-2 low-s normalization (required for `ecrecover`
    compatibility — the EVM rejects high-s signatures).

11. **NSM attestation requested.** The enclave asks the NSM for an attestation
    document with:
    - `user_data` = `SHA256(l1_head || l2_pre_root || l2_pre_block_number_be || l2_post_root || l2_post_block_number_be || rollup_config_hash)`
    - `nonce` = the caller-supplied nonce
    - `public_key` = the ephemeral secp256k1 public key (33-byte compressed)

12. **Response returned.** The enclave sends back `TransitionPublicValues`, the raw attestation
    document bytes, and the secp256k1 signature.

13. **Host-side verification.** Back in the worker (on the host), if production PCRs
    are configured:
    - Parse the COSE_Sign1 attestation document
    - Verify the P-384 signature against the leaf certificate
    - Check the certificate chain up to the hardcoded AWS Nitro Root CA
    - Validate certificate `notBefore`/`notAfter` periods
    - Check PCR0/1/2 match expected values
    - Verify `user_data` matches the computed hash from `TransitionPublicValues`
    - Verify the nonce matches what was sent
    - Extract the public key from the attestation and verify the secp256k1 signature

14. **Public values validated.** The worker checks that `TransitionPublicValues` fields match
    the job's expected `root_claim`, `l2_block_number`, `l1_head`, and `rollup_config_hash`.

15. **Proof submitted.** The worker posts `ProofData::Nitro { attestation, signature }`
    back to the `prover-service`.

---

## Attestation: Two-Layer Security

The nitro worker produces **two independent signatures** for every proof. Understanding
why both exist and how they bind together is crucial.

### Layer 1: NSM Attestation (P-384 ECDSA)

The Nitro Security Module signs a COSE_Sign1 document using P-384 ECDSA. This
signature is backed by a certificate chain rooted at the AWS Nitro Root CA. It proves:

- **Code identity:** PCR0/1/2 in the attestation identify _exactly which enclave
  image_ produced this document.
- **Computation output:** The `user_data` field contains
  `SHA256(l1_head || l2_pre_root || l2_pre_block_number_be || l2_post_root || l2_post_block_number_be || rollup_config_hash)`,
  binding the attestation to the specific state transition.
- **Key certification:** The `public_key` field contains the enclave's ephemeral
  secp256k1 public key, certifying that this key was generated inside this specific
  enclave.
- **Freshness:** The `nonce` field contains a caller-supplied random value, preventing
  replay of old attestations.

### Layer 2: Secp256k1 Signature (ECDSA)

The enclave signs `keccak256(l2_post_root || l2_block_number_be || rollup_config_hash)`
with the ephemeral secp256k1 key. This signature is:

- **EVM-native:** Verifiable via `ecrecover` (3,000 gas) — orders of magnitude cheaper
  than verifying P-384 on-chain.
- **The on-chain proof artifact:** `NitroProofVerifier.verify()` uses `ecrecover` to
  check this signature.

### How They Bind Together

The two layers are linked through the **key registration** step:

1. During key registration, the NSM attestation's `public_key` field certifies which
   secp256k1 key belongs to which enclave (with specific PCRs).
2. `NitroAttestationVerifier` verifies the full attestation (P-384 sig, cert chain,
   PCRs) and extracts the public key.
3. `NitroEnclaveKeyRegistry` stores the key as `Active`.
4. During proof verification, `NitroProofVerifier` uses `ecrecover` on the secp256k1
   signature and checks that the recovered key is registered.

This design separates the **expensive operation** (P-384 attestation verification +
cert chain validation, done once at key registration) from the **cheap operation**
(secp256k1 ecrecover, done for every proof).

> **Base comparison:** Base's op-enclave uses the same two-layer design — one P-384
> attestation at key registration, then secp256k1 `ecrecover` for every state root
> proposal. Base's enclave is written in Go; World Chain's is Rust using the Kona
> derivation pipeline. The NSM API calls and signing/attestation pattern are
> functionally identical.

---

## Commitments: `user_data` vs `signing_commitment`

There are two hash functions because they serve different purposes and face different
constraints:

### `range_user_data` — SHA-256

```
SHA256(l1_head || l2_pre_root || l2_pre_block_number_be || l2_post_root || l2_post_block_number_be || rollup_config_hash)
```

- **Used in:** The `user_data` field of the NSM attestation document.
- **Hash algorithm:** SHA-256, because the NSM device accepts arbitrary bytes in
  `user_data` and SHA-256 is standard for non-EVM contexts.
- **Includes all transition public values:** Yes — the full attestation commits to the L1
  derivation context and the complete L2 state transition.
- **Verified:** Host-side only (during `parse_check_and_verify`). Not used on-chain
  for per-proof verification.

### `signing_commitment` — keccak256

```
keccak256(l2_post_root || l2_block_number_be || rollup_config_hash)
```

- **Used in:** The message signed by the ephemeral secp256k1 key.
- **Hash algorithm:** keccak256, because `ecrecover` operates on keccak256 hashes
  natively, and the EVM precompile for keccak256 costs only 30 gas + 6 gas/word.
- **Omits `l2_pre_root`:** Yes — this makes the commitment reconstructable from
  minimal post-state fields. The on-chain verifier only needs the claimed output state,
  not the input state, to verify the signature. The pre-root is implicitly committed
  via the fault proof's `parentRef`.
- **Verified:** On-chain in `NitroProofVerifier.verify()` via `ecrecover`.

### Why Two?

The NSM attestation provides a complete audit trail (pre-root + post-root), verified
host-side with full cert chain validation. The secp256k1 signature provides a minimal,
EVM-efficient proof artifact. They commit to overlapping but different data because
their verification contexts have different requirements.

---

## PCR Measurements

### What Gets Measured

PCR values are SHA-384 hashes (48 bytes each) produced by `nitro-cli build-enclave`
when building the Enclave Image File (EIF):

| PCR | Measures | Changes When... |
|-----|----------|-----------------|
| PCR0 | Entire EIF (kernel + ramdisk + bootstrap) | Any part of the enclave image changes |
| PCR1 | Kernel + bootstrap | Kernel or bootstrap changes (not app code) |
| PCR2 | Application code | Application binary changes |

PCR0 is the **primary identity** — it uniquely identifies the complete enclave image.

> **Base comparison:** Base's `SystemConfigGlobal` registers only **PCR0** (stored as
> `keccak256(pcr0)` in a boolean mapping). World Chain uses the full **PCR0+PCR1+PCR2
> triple**, which is more precise — PCR1 and PCR2 let you distinguish kernel-only
> changes from application-only changes. The trade-off: approving a new World Chain
> PCR triple requires three values to be captured and submitted vs Base's single PCR0.

### Production vs Placeholder Mode

**Placeholder PCRs** (`ExpectedPcrs::PLACEHOLDER`): All 48 bytes set to zero for all
three PCRs. When the worker sees placeholder PCRs:
- **All attestation verification is skipped** — no P-384 signature check, no cert
  chain validation, no user_data check, no nonce check, no signature verification.
- This is **dev/test mode only**.

**Production PCRs**: Real 48-byte values provided via `--pcr0`, `--pcr1`, `--pcr2`
CLI flags or environment variables. When set:
- Full 5-check attestation verification is performed (see
  [Attestation Verification](#host-side-verification-5-checks) below).
- Mismatched PCRs cause the proof to be rejected.

### What Happens If PCRs Are Wrong

If you deploy a new enclave image but forget to update the expected PCRs:
- **Host-side:** `parse_check_and_verify` will fail — the PCRs in the attestation
  won't match the expected values.
- **On-chain:** If the old PCR set is still approved, keys from the new enclave can't
  register. If the new PCR set isn't approved, `registerKey` will revert.

---

## On-Chain Verification Flow

### Key Registration (One-Time Per Enclave Boot)

```
registerKey(attestationTbs, signature)
        │
        ▼
NitroEnclaveKeyRegistry
        │ calls
        ▼
NitroAttestationVerifier.verifyAttestation(attestationTbs, signature)
        │
        ├─ 1. Parse COSE_Sign1 structure (CBOR decode)
        ├─ 2. Extract certificate chain from attestation payload
        ├─ 3. Validate cert chain via ICertManager
        │      └─ Check each cert's validity period (notBefore / notAfter)
        │      └─ Verify signatures up to pinned AWS Nitro Root CA
        ├─ 4. Verify P-384 ECDSA signature against leaf cert's public key
        ├─ 5. Extract PCR0, PCR1, PCR2 from attestation payload
        │      └─ Hash each with keccak256
        │      └─ Check triple is in approvedPCRSets allowlist
        └─ 6. Extract and return the secp256k1 public key from public_key field
                    │
                    ▼
        NitroEnclaveKeyRegistry stores keccak256(publicKey) as Active
```

### Proof Verification (Every Proof)

```
verify(rootId, proof)
        │
        ▼
NitroProofVerifier
        │
        ├─ 1. ABI-decode proof:
        │      (domainHash, parentRef, l1OriginHash, l1OriginNumber,
        │       rollupConfigHash,
        │       l2PostRoot, l2BlockNumber, signature, expectedPublicKey)
        │
        ├─ 2. Reconstruct rootId from proof fields
        │      └─ Assert reconstructed == supplied rootId
        │
        ├─ 3. Check expectedPublicKey is registered in NitroEnclaveKeyRegistry
        │      └─ isKeyRegistered(expectedPublicKey) must return true
        │
        ├─ 4. Compute signing commitment:
        │      keccak256(l2PostRoot || l2BlockNumber || rollupConfigHash)
        │
        ├─ 5. ecrecover(commitment, signature) → recovered address
        │      └─ EIP-2 low-s check: reject if s > secp256k1n/2
        │
        ├─ 6. Compare recovered address with keccak256(expectedPublicKey[1:65])[12:]
        │      └─ Must match
        │
        └─ 7. Return true (or false on any failure — never reverts)
```

> **Base comparison:** Base's `SystemConfigGlobal.registerSigner()` performs the same
> steps 1–5 (attestation validation via `NitroValidator`), then derives an Ethereum
> address from the attestation's `public_key` field:
> ```
> publicKeyHash = keccak256(publicKey[1:])  // skip 0x04 prefix
> enclaveAddress = address(uint160(publicKeyHash))
> validSigners[enclaveAddress] = true
> ```
> World Chain takes the same approach but stores `keccak256(fullPublicKey)` →
> `KeyStatus` in `NitroEnclaveKeyRegistry` instead of `address → bool` in a flat map,
> enabling the 3-state lifecycle and making the full uncompressed key available for
> verification. The on-chain `ecrecover` check is identical in both systems.

---

## Key Lifecycle

The full lifecycle from enclave build to key revocation:

### 1. Build the Enclave Image

```bash
nitro-cli build-enclave --docker-uri <image> --output-file enclave.eif
```

This outputs PCR0, PCR1, and PCR2 values for the built image.

### 2. Approve PCR Set On-Chain

The contract owner calls `NitroAttestationVerifier.approvePCRSet(pcr0, pcr1, pcr2)`.
This adds the PCR triple to the allowlist. Without this, `registerKey` will fail
because the attestation's PCRs won't be recognized.

### 3. Pre-Warm CertManager

See [CertManager Pre-Warm](#certmanager-pre-warm) below. The AWS Nitro CA certificate
chain must be cached in `CertManager` before any attestation can be verified on-chain.

### 4. Start the Enclave

The enclave boots and:
- Calls `nsm_init()` to get the NSM device descriptor
- Requests 32 bytes of hardware entropy via `NsmRequest::GetRandom`
- Derives an ephemeral secp256k1 signing key from that entropy
- Stores the key in a `OnceLock<SigningKey>` (immutable for the enclave's lifetime)
- Begins listening on vsock port 5005

### 5. Register the Key

The operator sends a `PublicKey` request to the enclave (via the
`world-chain-prover-nitro` CLI). The enclave returns an NSM attestation with the
ephemeral public key in the `public_key` field.

The operator calls `NitroEnclaveKeyRegistry.registerKey(attestationTbs, signature)`:
- `NitroAttestationVerifier` verifies the attestation (P-384 sig, cert chain, PCRs)
- Extracts the secp256k1 public key
- Key state transitions from `Unknown` → `Active`

### 6. Prove

The enclave can now produce range proofs whose secp256k1 signatures will be accepted
by `NitroProofVerifier`, because the signing key is registered.

### 7. Revoke (If Needed)

The owner can call `NitroEnclaveKeyRegistry.revokeKey(publicKey)`:
- Key state transitions from `Active` → `Revoked`
- **Revocation is permanent** — the same key cannot be re-registered
- Subsequent proofs signed by this key will fail verification

Revocation is necessary when an enclave is decommissioned or compromised.

> **Base comparison:** Base's `SystemConfigGlobal` uses a simple `mapping(address =>
> bool) validSigners`. Deregistration is `delete validSigners[addr]` — the address
> can be re-added later with a new attestation. World Chain's `KeyStatus.Revoked` is
> permanent; a revoked key hash can never be reactivated. This closes the replay
> attack window (a captured attestation could re-register a deleted Base key, but not
> a World Chain `Revoked` one).

---

## CertManager Pre-Warm

### The Chicken-and-Egg Problem

`NitroAttestationVerifier` delegates X.509 certificate chain validation to
`CertManager`. But `CertManager` must have the AWS Nitro CA certificates **already
cached** before it can validate any attestation. And to get a real certificate chain
to cache, you need a real attestation document from a running enclave.

So: you can't register a key without cached certs, and you can't cache certs without
an attestation document.

### The Solution

Use a **bare attestation** (no user_data, no nonce, no public_key) purely to extract
and cache the certificate chain:

#### Step 1: Get a Bare Attestation

```bash
world-chain-prover-nitro get-attestation
```

This sends a `GetAttestation` request to the enclave, which issues a bare NSM
attestation and returns the raw COSE_Sign1 bytes. This attestation isn't used for
verification — only for extracting the embedded cert chain.

#### Step 2: Extract and Cache the Certificate Chain

```bash
node hinted_attestation_calls.js prepare <attestation_hex>
```

This script:
1. Parses the COSE_Sign1 attestation to extract the X.509 certificate chain
2. Calls `CertManager.verifyCACert()` and related functions for each certificate
3. This is expensive (~63M gas total) but only needs to be done once
4. The certs are now cached in `CertManager`'s storage

#### Step 3: Approve the PCR Set

```bash
node hinted_attestation_calls.js approvePCRSet <pcr0> <pcr1> <pcr2>
```

The contract owner approves the PCR triple in `NitroAttestationVerifier`.

#### Step 4: Register Keys

Now `registerKey` calls will succeed because `CertManager` can validate the cert chain
and the PCR set is approved.

> **Base comparison:** Base uses the same `CertManager` pre-warm workflow (it's their
> contract). Their tooling (`tools/hinted_attestation_calls.js`) is the reference
> implementation; World Chain uses the same script. The hinted P-384 verification
> (where modular inverses are computed off-chain and verified on-chain) was introduced
> by Base in [`nitro-validator` PR #28](https://github.com/base/nitro-validator/pull/28)
> after the Fusaka upgrade raised `MODEXP` pricing enough that the old fully on-chain
> P-384 path no longer fit in a block. Both systems now require ~27 KB of hint calldata
> per P-384 signature, split across 5 transactions for cold cert-chain warming (total
> ~41M gas) and 1 transaction for warm attestation validation (~13.8M gas).

---

## Kubernetes Deployment

### Pod Structure

The `nitro-worker` pod has two containers. There is no vsock-proxy — the enclave
makes no outbound AWS API calls. If KMS key persistence is added later, a vsock-proxy
will be needed (see the [KMS improvement proposal](#future-improvement-kms-based-key-persistence)).

#### 1. `nitro-worker` (Main Container)

- Runs the `nitro-worker` binary (polls prover-service, builds witnesses, submits proofs)
- Reads the enclave CID from `/run/nitro-shared/enclave-cid` (written by the launcher)
- Connects to the enclave over vsock using that CID

#### 2. `enclave-launcher` (Sidecar)

- Runs `nitro-cli run-enclave` with the pre-built EIF baked into the container image
- Extracts the enclave's assigned CID via `nitro-cli describe-enclaves`
- Writes the CID to `/run/nitro-shared/enclave-cid` on the shared `emptyDir` volume
- Blocks on `nitro-cli console` to keep the container alive and stream enclave logs



### Required Resources

```yaml
resources:
  limits:
    hugepages-1Gi: <size>          # Nitro Enclaves require hugepages
    aws.ec2.nitro/nitro_enclaves: 1 # Device plugin for enclave access
```

- **Hugepages (1 GiB):** Nitro Enclaves allocate memory from hugepages, not regular
  memory.
- **Nitro Enclave device:** The `aws.ec2.nitro/nitro_enclaves` resource request tells
  the Kubernetes device plugin to expose the `/dev/nitro_enclaves` device to the pod.
- The underlying EC2 instance must be a Nitro Enclave-capable instance type (e.g.,
  `m5.xlarge` or larger with enclave support enabled).

### Shared Volume

A shared `emptyDir` volume mounted at `/run/nitro-shared` in both the `nitro-worker`
and `enclave-launcher` containers. The launcher writes the enclave CID; the worker
reads it.

---

## Dev/Test Mode

### Placeholder PCRs

When `--pcr0`, `--pcr1`, `--pcr2` are all set to zero (or omitted, defaulting to
`ExpectedPcrs::PLACEHOLDER`), the worker runs in **dev/test mode**:

- **All attestation verification is skipped:**
  - No COSE_Sign1 P-384 signature verification
  - No certificate chain validation
  - No PCR matching
  - No `user_data` hash verification
  - No nonce verification
  - No secp256k1 signature verification
- The enclave still produces real attestations and signatures, but the host doesn't
  check them.

### Debug Mode Enclave

Adding `--debug-mode` to `nitro-cli run-enclave` enables:
- Console access to the enclave (for debugging)
- Attestation documents will have **all-zero PCRs** (the NSM sets them to zero in
  debug mode)

**Never use debug mode in production** — the all-zero PCRs mean any attestation from
a debug enclave is indistinguishable from a forged one.

### When to Use Dev/Test Mode

- Local development without Nitro hardware
- CI/CD pipelines testing the worker logic
- Integration tests that don't need real attestation guarantees

---

## Configuration Reference

### `nitro-worker` (Host Binary)

| Flag / Env Var | Description | Default |
|----------------|-------------|---------|
| `--prover-service-url` / `PROVER_SERVICE_URL` | URL of the prover-service API | Required |
| `--l2-rpc` / `L2_RPC_URL` | World Chain L2 RPC endpoint | Required |
| `--l1-rpc` / `L1_RPC_URL` | Ethereum L1 RPC endpoint | Required |
| `--l1-beacon-rpc` / `L1_BEACON_RPC_URL` | Ethereum L1 Beacon API endpoint | Required |
| `--network` / `NETWORK` | `worldchain` or `worldchain-sepolia` | `worldchain` |
| `--rollup-config` / `ROLLUP_CONFIG` | Path to rollup config JSON file | — |
| `--rollup-config-hash` / `ROLLUP_CONFIG_HASH` | Rollup config hash override | — |
| `--block-interval` / `BLOCK_INTERVAL` | L2 blocks per proof (must match contract) | Required |
| `--enclave-cid` / `ENCLAVE_CID` | vsock CID of the enclave | `16` |
| `--enclave-port` / `ENCLAVE_PORT` | vsock port the enclave listens on | `5005` |
| `--pcr0` / `PCR0` | Expected PCR0 (hex, 48 bytes) | All zeros (placeholder) |
| `--pcr1` / `PCR1` | Expected PCR1 (hex, 48 bytes) | All zeros (placeholder) |
| `--pcr2` / `PCR2` | Expected PCR2 (hex, 48 bytes) | All zeros (placeholder) |
| `--worker-id` | Unique identifier for this worker instance | Required |
| `--poll-interval-seconds` / `POLL_INTERVAL_SECONDS` | Seconds between prover-service polls | `10` |
| `--max-concurrent-jobs` | Max jobs proved in parallel | `1` |

### `world-chain-nitro-enclave` (Enclave Binary)

| Env Var | Description | Default |
|---------|-------------|---------|
| `NITRO_VSOCK_PORT` | vsock port to listen on | `5005` |

### Wire Protocol

| Field | Format |
|-------|--------|
| Message length | 4 bytes, big-endian `u32` |
| Message body | CBOR-encoded `EnclaveRequest` or `EnclaveResponse` |

### `world-chain-prover-nitro` CLI Commands

| Command | Description |
|---------|-------------|
| `hash-rollup-config` | Print the rollup config hash |
| `witness` | Build and serialize a witness to a file without proving |
| `prove` | Generate witness and send to a running enclave for attested proving |
| `get-attestation` | Fetch a bare NSM attestation (for CertManager pre-warm) |

> **Note:** `nitro-worker` (the long-running production worker that polls the prover-service)
> is a separate binary from `world-chain-prover-nitro` (the one-shot CLI tool above).

---

## Frequently Asked Questions

### Q: Why are PCRs passed into `verifyAttestation` if they are already included in the attestation document?

**A:** The attestation document contains the *actual* PCR values from the running
enclave. But the verifier needs *expected* PCR values to compare against — without
supplying expected PCRs, there is nothing to verify: any enclave image would be
accepted. The caller provides expected PCRs so the contract can assert the enclave ran
exactly the image the operator approved. The on-chain `NitroAttestationVerifier`
maintains an `approvedPCRSets` allowlist; it hashes the PCRs extracted from the
attestation and checks them against this set.

### Q: How does an upgrade to a new enclave version (with new PCRs) work?

**A:** The upgrade is a rolling process:

1. Build a new EIF → get new PCR0/1/2 measurements.
2. Call `approvePCRSet(newPcr0, newPcr1, newPcr2)` — both old and new PCR sets are
   now approved simultaneously.
3. Deploy new enclaves; each one registers its ephemeral key via `registerKey` (which
   succeeds because the new PCRs are approved).
4. Once all enclaves have migrated, call `revokePCRSet(oldPcr0, oldPcr1, oldPcr2)` —
   this disallows future key registrations for the old image. Already-registered keys
   from the old image remain valid until individually revoked if needed.

This allows zero-downtime upgrades with a window where both old and new enclave
images are active.

### Q: Does `NitroAttestationVerifier` need PCRs in its constructor?

**A:** No. PCR management is fully dynamic — PCR sets are added and removed at runtime
via `approvePCRSet` / `revokePCRSet`. No PCRs are baked into the contract constructor.
This allows the owner to approve new enclave images and retire old ones without
redeploying the verifier contract.

### Q: Why store revoked keys in `NitroEnclaveKeyRegistry` instead of just deleting them?

**A:** To prevent replay attacks. If a key were simply deleted on revocation, an
attacker who captured that key's original attestation document could re-submit it to
`registerKey` and restore the key. The `Revoked` state is permanent — once revoked, a
key can never be re-registered regardless of whether a valid attestation document for
it is available. The lifecycle is strictly `Unknown → Active → Revoked` with no path
back.

### Q: Why is there an `Unknown` state in the key lifecycle enum? Isn't just Active/Revoked enough?

**A:** Solidity enums default to their zero value. Having `Unknown` as the zero value
makes it explicit when a key has never been registered at all, distinguishing it from
`Active`. It also avoids the footgun where the zero value silently means "active"
before any registration occurs. Gas-wise, checking against a 3-value enum is identical
to a 2-value one — both fit in a single storage slot byte.

### Q: When a PCR set is revoked, should all keys registered under those PCRs be automatically revoked too?

**A:** This was explicitly considered and decided against. Revoking a PCR set is a
**forward-looking** operation — it prevents *new* keys from registering under that
image. Existing registered keys were individually verified at registration time and are
tracked separately. Mass-revoking all keys on PCR revocation would require either an
on-chain `PCR → [keys]` mapping (which conflicts with multi-instance support) or an
O(n) loop over all keys. The security posture is: if you need to revoke all keys from
a compromised image, call `revokeKey` for each individually. This is noted as a
security consideration in the contract's NatSpec.

### Q: Can you build an EIF on a regular machine, or do you need a Nitro-capable EC2 host?

**A:** `nitro-cli build-enclave` requires the Nitro CLI, which only runs on Amazon
Linux on Nitro-capable EC2 instances — not in standard GitHub Actions runners or local
machines. The build pipeline works around this:

1. CI builds the enclave binary Docker image normally (any runner).
2. A dedicated Nitro-capable EC2 runner runs `nitro-cli build-enclave` to produce the
   EIF file.
3. The EIF is baked into a second Docker image (`Dockerfile.eif`) published to GHCR.
4. The `enclave-launcher` sidecar in Kubernetes pulls this image and runs
   `nitro-cli run-enclave` using the EIF baked inside it.

### Q: How does the enclave-launcher share the enclave CID with the main nitro-worker container?

**A:** The CID is dynamic — `nitro-cli run-enclave` allocates it at runtime and it
cannot be hardcoded because multiple enclave instances could run on the same node. The
launcher extracts the CID after enclave start using `nitro-cli describe-enclaves`, then
writes it to `/run/nitro-shared/enclave-cid` (a shared `emptyDir` volume). The main
`nitro-worker` container polls this file at startup before connecting over vsock.

### Q: Does the enclave have any network access?

**A:** No. AWS Nitro Enclaves are fully network-isolated — no inbound or outbound
TCP/IP. The only channel is vsock, which connects the enclave to its parent EC2
instance. Currently there is no vsock-proxy in the deployment, so the enclave has
no outbound connectivity at all. If KMS-based key persistence is implemented in the
future, a vsock-proxy sidecar will be re-added to bridge vsock to AWS endpoints (e.g. `kms.us-east-1.amazonaws.com:443` for KMS).

---

## Host-Side Verification (5 Checks)

When production PCRs are configured, the host performs five verification checks on
every attestation:

1. **PCR + user_data invariants:** PCR0/1/2 match expected values; `user_data` matches
   `SHA256(l1_head || l2_pre_root || l2_pre_block_number_be || l2_post_root || l2_post_block_number_be || rollup_config_hash)`.
2. **COSE_Sign1 signature:** P-384 ECDSA signature verified against the leaf
   certificate's public key.
3. **Root certificate:** The root of the certificate chain matches the hardcoded
   `AWS_NITRO_ROOT_CA_PEM` constant (sourced from the official AWS Nitro Enclaves
   root certificate archive).
4. **Certificate validity:** Every certificate in the chain is checked for valid
   `notBefore` and `notAfter` dates.
5. **Nonce freshness:** The nonce in the attestation matches the nonce the host
   generated, preventing replay of old attestation documents.

The codebase provides three verification depths for different call sites:
- `parse_check_and_verify` — full verification (all 5 checks)
- `verify_cose_sign1_signature` — signature + cert chain only
- `verify_pcrs_only` — just PCR matching (lightweight check)

---

---

## Future Improvement: KMS-Based Key Persistence

> **Status: not implemented.** This section describes a planned improvement.

### Problem

The current ephemeral-key design requires on-chain re-registration every time the
enclave restarts, creating a brief liveness gap. Base's RSA-sealing approach avoids
this but couples the enclave restart to the proposer process being online to
orchestrate the handoff.

### Proposed solution: attestation-based KMS key policy

AWS KMS supports **PCR-conditioned key policies**: KMS verifies the NSM attestation
document submitted alongside an API call and only grants access if the PCR values in
the attestation match the conditions in the key policy. This lets KMS act as a
hardware-enforced escrow for the secp256k1 signing key.

#### One-time setup

Create a KMS key with PCR conditions:

```json
{
  "Condition": {
    "StringEqualsIgnoreCase": {
      "kms:RecipientAttestation:PCR0": "<hex PCR0>",
      "kms:RecipientAttestation:PCR1": "<hex PCR1>",
      "kms:RecipientAttestation:PCR2": "<hex PCR2>"
    }
  }
}
```

Only an enclave whose NSM attestation contains those exact PCR values can use this
key — enforced by KMS, not by application code.

#### First boot — sealing the signing key

```
Enclave
  │
  ├─ Generate secp256k1 signing key from NSM entropy (as today)
  ├─ Register new key on-chain (once, as today)
  │
  ├─ Call kms:GenerateDataKey  ← NSM attestation attached automatically by AWS SDK
  │   KMS verifies attestation + PCR conditions → returns plaintext + encrypted data key
  │
  ├─ Encrypt secp256k1 key bytes with plaintext data key (AES-256-GCM)
  ├─ Wipe plaintext data key from memory
  │
  └─ Store { encrypted_data_key || encrypted_signing_key } to S3 / K8s Secret
```

#### Subsequent boots — unsealing the signing key

```
Enclave
  │
  ├─ Read { encrypted_data_key, encrypted_signing_key } from storage
  │
  ├─ Call kms:Decrypt(encrypted_data_key)  ← NSM attestation attached
  │   KMS verifies attestation + PCR conditions → returns plaintext data key
  │
  ├─ Decrypt secp256k1 key bytes
  └─ Same key as before → no on-chain re-registration needed
```

#### PCR upgrade flow

1. Add new PCRs to the KMS key policy (both old and new approved simultaneously).
2. Deploy new enclave — on first boot it reads the existing ciphertext; KMS now
   accepts the new PCRs, so it can unseal → same secp256k1 key is restored.
3. Remove old PCRs from the key policy.

#### Comparison

| | KMS (proposed) | Base RSA-sealing | Current (ephemeral) |
|---|---|---|---|
| Key survives enclave restart | ✅ | ✅ | ❌ |
| Key survives proposer restart | ✅ | ❌ | — |
| Trust root | AWS KMS + NSM | Proposer process | — |
| PCR binding | KMS key policy (AWS-enforced) | PCR0 check in Go code | — |
| Ciphertext storage | S3 / K8s Secret | Proposer RAM | — |
| Proposer involvement on restart | None | Must orchestrate handoff | — |
| On-chain re-registration on restart | ❌ not needed | ❌ not needed | ✅ required |

#### What needs to be built

The vsock-proxy sidecar was removed because the enclave currently makes no outbound
calls. To implement KMS key persistence, the following are needed:

1. `aws-sdk-kms` (Rust) + `aws-nitro-enclaves-sdk-rust` in the enclave binary for
   attestation-attached API calls.
2. A KMS key with the PCR-conditioned key policy above.
3. An IAM role for the enclave pod with `kms:GenerateDataKey` + `kms:Decrypt`
   permissions, bound via IRSA / Pod Identity.
4. A storage location for the ciphertext blob (S3 bucket or K8s Secret), readable
   by the enclave at startup.
