|             |                                                                                                                                                                                                                                                                                              |
| ----------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| WIP         | 1001                                                                                                                                                                                                                                                                                         |
| Title       | Native Account Abstraction via World ID Accounts                                                                                                                                                                                                                                             |
| Description | A draft core proposal defining native World ID Account Abstraction: a precompile-managed account creation/recycling flow keyed by World ID nullifiers, delegated multi-auth key commitments, and a new EIP-2718 typed transaction (0x6f) with sender derivation and signature encoding rules |
| Author      | 0xOsiris                                                                                                                                                                                                                                                                                     |
| Status      | Draft                                                                                                                                                                                                                                                                                        |
| Category    | Core                                                                                                                                                                                                                                                                                         |
| Created     | 2026/03/27                                                                                                                                                                                                                                                                                   |
| Requires    | [EIP-2718](https://eips.ethereum.org/EIPS/eip-2718), [EIP-7702](https://eips.ethereum.org/EIPS/eip-7702), [ERC-4337](https://eips.ethereum.org/EIPS/eip-4337), [EIP-1559](https://eips.ethereum.org/EIPS/eip-1559)                                                                           |

## Abstract

A *World ID Account* is a account whose `20 byte` public key identifier is a derivation over a [World ID](https://worldcoin.org/world-id) v4 [OPRF](https://datatracker.ietf.org/doc/rfc9497/) nullifier. We extend [EIP-2718](https://eips.ethereum.org/EIPS/eip-2718) with a typed transaction envelope indicated via the type flag byte (`0x6F`). A transaction of type `0x6F` uniquely defines the signature verification algorithm by which *World ID Account*s are authenticated. The signatories over any *World ID Account* are delegated via a public signal of World ID uniqueness proof, and may be rotated via a single proof. 

A *World ID Account* is ephemeral by design. It may be provably destroyed and recreated via a proof whose corresponding `action` is a commitment over a `WORLD_ID_ACCOUNT_GENERATION_NONCE`. A global monotonic `WORLD_ID_ACCOUNT_NONCE` anchors a *World ID Account*s history over `Relying Party Id = 480` which dually represents the current generations account nonce in the global [Merkle Patricia Trie (MPT)](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/) state.

A *World ID Account* preserves unlinkability of between its `WORLD_ID_ACCOUNT_DELEGATE`, all WorldIDRegistry authenticators corresponding to the authenticated `leaf_index` in the tree, and all delegated signing keys (assuming the World ID authenticator does not intentionally authenticate their linkage).

<!-- 
The proof's `signal` commits to the authorized key set. Accounts are ephemeral — they can be destroyed and recreated at a new address, preserving only a monotonic nonce. -->

---

<details>
  <summary><h2><br> Design Rationale and Motivation </h2></summary>

#### Economic Security
On **[World Chain](https://worldcoin.org/world-chain)** World ID holders receive full gas sponsorship on transactions up to a threshold of 300 Txs per day. Sponshorship is facilitaded via [ERC-4337](https://eips.ethereum.org/EIPS/eip-4337) where Bundlers are compensated off chain for all sponsored traffic. This sponsorship provides great UX for humans on chain, but leaves the chain in a vulnerable and economically unsustainable position where external trafffic cannot be effectively priced with respect to relative load and computation without affecting sponsorship for verified users.

World Chain is positioned to take on spam, agentic traffic, and high frequency financial markets while concurrently providing the best possible UX to humans. The solution is to leverage World ID as an account level primitive by which we bifurcate the current [EIP-1559](https://eips.ethereum.org/EIPS/eip-1559) Fee Market. Allowing the fee market(s) to run assymptotically of a nash equilibria anchored in economic game theory.

#### Computational Sustainability & World Chain UX
Enshrining a smart accounts authenticators as protocol level signature verification algorithms we can in parallel optimize transaction gas usage, provide simple api for multi-auth via familiar authenticators ([Passkey](https://www.w3.org/TR/webauthn-3/) [WebAuthn](https://www.w3.org/TR/webauthn-3/), [P256](https://neuromancer.sk/std/nist/P-256), [ECDSA](https://en.wikipedia.org/wiki/Elliptic_Curve_Digital_Signature_Algorithm), [EdDSA](https://en.wikipedia.org/wiki/EdDSA)) natively, minimize confirmation time, and provide the requisite pre-conditions to differentiate a human from bot at the account level.

#### Abstract Delegated Authenticator
By enshrining a World ID as a native account architecture over a nonce commiting the complete history over a POH nullifier. We functionally transform the protocol into a native anonymous address book. Authenticated signatories over the *World ID Account* form the registrar by which servers and apps can gate POH over Humans, and Agents. 

Noting relevance to [EIP-8004](https://eips.ethereum.org/EIPS/eip-8004), [Machine Payments Protocol](https://github.com/AnotherPianist/eip-machine-payments), and [x402](https://www.x402.org/) worth further exploration.

#### Typed Transaction Values

We select `0x1D` noting typed identifiers around bounded in range `[0, 0x7F]`. We exclude `[0x70, 0x7D]` to offset buffer `15` positions for potential OP native transaction types e.g. (`Deposit 0x7E`). Preferring a value in the upper range as to minimize risk of collision with future Ethereum L1 typed transactions introduced. We leave `0x7F` open maintaining future compatibility for usage in variable-length encoding scheme.

---

</details>

*WIP-1001 attempts to lay the requisite bricks which make such improvements current, and future possible.*

---

### Alternatives Considered
|    Name                                                                  |
| ---------------------------------- 
|       [Priority Blockspace for Humans](https://github.com/worldcoin/world-chain/blob/main/specs/pbh/overview.md)              |
|       [AgentBook/AgentKit and Extending Legacy Address Book]() |
|       [World ID Singleton 7702 Delegation](https://github.com/5afe/safe-eip7702/blob/main/) |
|       [Frame Transactions](https://eips.ethereum.org/EIPS/eip-8141) |


## Proposal

### Constants

| Name                               | Value                                                                                                                 | Description                                                               |
| ---------------------------------- | --------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `WORLD_TX_TYPE`                    | `0x6f`                                                                                                                | [EIP-2718](https://eips.ethereum.org/EIPS/eip-2718) transaction type byte |
| `WORLD_ID_SIGN`                    | `3`                                                                                                                   | Signature scheme identifier                                               |
| `WORLD_CHAIN_RP_ID`                | `480`                                                                                                                 | World Chain's registered RP ID                                            |
| `MAX_AUTHORIZED_KEYS`              | `20`                                                                                                                  | Maximum delegated keys per account                                        |
| `WORLD_ID_ACCOUNT_FACTORY`         | `0x000000000000000000000000000000000000001D`                                                                          | Stateful precompile address                                               |
| `WORLD_ID_ACCOUNT_DELEGATE`        | `bytes20(keccak256(POH_ACCOUNT_GENERATION_NULLIFIER))`                                                                | A native World ID account                                                 |
| `POH_ACCOUNT_NULLIFIER`            | nullifier over `action = keccak256('WORLD_ID_ACCOUNT' \|\| WORLD_CHAIN_RP_ID)`                                        | deterministic account anchor                                              |
| `POH_ACCOUNT_GENERATION_NULLIFIER` | nullifier over `action = keccak256(WORLD_CHAIN_RP_ID \| WORLD_ID_ACCOUNT_NONCE \| WORLD_ID_ACCOUNT_GENERATION_NONCE)` | monotonic generational nullifier over account recycle count               |
| `PEDERSEN_N`                       | `32`                                                                                                                  | Padded vector length: $2^{\lceil \log_2 n \rceil}$ where $n = $ `MAX_AUTHORIZED_KEYS` |
| `PEDERSEN_GENERATORS`              | `𝐆 = [G_0, …, G_{N-1}], 𝐇 = [H_0, …, H_{N-1}], B, Q` via `HashToCurve`                                            | BN254 G₁ generators for Pedersen vector commitments and Bulletproofs IPA  |
| `NULLIFIER_G`                      | `HashToCurve("WorldChain.Nullifier.G")`                                                                              | BN254 G₁ generator for nullifier Pedersen commitments (§3.5.6)           |
| `NULLIFIER_H`                      | `HashToCurve("WorldChain.Nullifier.H")`                                                                              | BN254 G₁ blinding generator for nullifier commitments (§3.5.6)           |
| `NONCE_G`                          | `HashToCurve("WorldChain.EpochNonce.G")`                                                                             | BN254 G₁ generator for epoch nonce commitments (§3.6)                    |
| `NONCE_H`                          | `HashToCurve("WorldChain.EpochNonce.H")`                                                                             | BN254 G₁ blinding generator for epoch nonce (§3.6)                       |
| `EPOCH_LENGTH`                     | `43200`                                                                                                               | Blocks per epoch (~1 day at 2s block time)                               |
| `EPOCH_TX_LIMIT`                   | `300`                                                                                                                 | Maximum sponsored transactions per epoch                                 |

---

### Account Model

A World ID Account is an address derived from a proven **account nullifier**. A precompile at `WORLD_ID_ACCOUNT_FACTORY` manages global nullifier tracking over `rp_id = 480`.

| Nullifier    | Purpose                     | Action Derivation                                                  |
| ------------ | --------------------------- | ------------------------------------------------------------------ |
| **Creation** | track account recycle count | `keccak256("WORLD_ID_ACCOUNT_CREATION")`                           |
| **Account**  | pub key derivation          | `keccak256(WORLD_CHAIN_RP_ID \|\| world_id_nonce \|\| generation)` |


### WorldIDAccountFactory — Precompile

A stateful precompile deployed at `WORLD_ID_ACCOUNT_FACTORY`. It is the sole entry point for World ID Account lifecycle management: creation, recycling, and destruction. All state mutations require verification of World ID v4 [OPRF](https://datatracker.ietf.org/doc/rfc9497/) nullifier proofs against the on-chain `WorldIDRegistry` root.

---

### Formal Specification of Account and Creation Proofs

> Sections 3.3.1, and 3.3.2 provide a primer on the current model, and soundness guarantees of World ID 4.0 proofs via OPRF. Informed readers may feel free to skip directly to section 3.3.3.

#### 3.3.1 OPRF Nullifier Proof Format

An `OPRFNullifierProof` is a [Groth16](https://eprint.iacr.org/2016/260) proof over [BN254](https://hackmd.io/@jpw/bn254). The circuit verifies an oblivious PRF evaluation, credential validity, and Merkle membership in the `WorldIDRegistry`.

A proof `π` is submitted on-chain as `uint256[5]`:

```
zeroKnowledgeProof = [a, b₀, b₁, c, merkle_root]
```

- `(a, b₀, b₁, c)` — compressed Groth16 proof elements
- `merkle_root` — `WorldIDRegistry` Merkle root, validated on-chain via `WorldIDRegistry.isValidRoot()`

The `WorldIDVerifier` reconstructs **15 public inputs** from calldata and on-chain registry lookups, then invokes the Groth16 verifier:

```solidity
function verify(
    uint256 nullifier,          // circuit output: deterministic per (user, rp_id, action)
    uint256 action,             // RP-defined action scope (hashed)
    uint64  rpId,               // Relying Party identifier (480 for World Chain)
    uint256 nonce,              // per-request nonce
    uint256 signalHash,         // arbitrary public signal commitment (truncated 254 bits)
    uint64  expiresAtMin,       // minimum credential expiration timestamp
    uint64  issuerSchemaId,     // credential schema + issuer pair identifier
    uint256 credentialGenesisIssuedAtMin,
    uint256[5] calldata zeroKnowledgeProof
) external view;
```

| Index | Public Input                   | Source                                                   |
| ----- | ------------------------------ | -------------------------------------------------------- |
| 0     | `nullifier`                    | Caller (circuit output)                                  |
| 1     | `issuerSchemaId`               | Caller                                                   |
| 2–3   | `credentialIssuerPubkey.(x,y)` | `CredentialSchemaIssuerRegistry[issuerSchemaId]`         |
| 4     | `expiresAtMin`                 | Caller                                                   |
| 5     | `credentialGenesisIssuedAtMin` | Caller                                                   |
| 6     | `merkle_root`                  | `zeroKnowledgeProof[4]`, validated via `WorldIDRegistry` |
| 7     | `treeDepth`                    | `WorldIDRegistry.depth()`                                |
| 8     | `rpId`                         | Caller                                                   |
| 9     | `action`                       | Caller                                                   |
| 10–11 | `oprfPublicKey.(x,y)`          | `OprfKeyRegistry[rpId]`                                  |
| 12    | `signalHash`                   | Caller                                                   |
| 13    | `nonce`                        | Caller                                                   |
| 14    | `sessionId`                    | `0` (uniqueness proofs)                                  |

#### 3.3.2 Circuit Guarantees

The `OPRFNullifier` circuit proves knowledge of a private witness:

$$w = (s, \; k, \; p, \; \beta, \; R_{\mathrm{oprf}}, \; \mathit{cred}, \; \sigma_{\mathit{cred}})$$

where $s$ is the identity secret, $k$ is the authenticator key, and $p$ is the Merkle path.

satisfying:

**(G1) Merkle membership.** $k$ is a leaf in the `WorldIDRegistry` tree at the declared root $r$ and depth $d$:

$$\mathrm{MerkleVerify}(r, \; i, \; \mathrm{Poseidon}(k), \; p) = \mathsf{true}$$

where $i$ is the leaf index.

**(G2) Credential validity.** $\mathit{cred}$ bears a valid EdDSA-Poseidon2 signature $\sigma_{\mathit{cred}}$ over [BabyJubJub](https://eips.ethereum.org/EIPS/eip-2494) from the issuer identified by `issuerSchemaId`, and the credential has not expired.

**(G3) OPRF correctness.** The OPRF query $q$ was honestly derived from the user's leaf, the response $R_{\mathrm{oprf}}$ was evaluated under the registered OPRF key for `rpId`, and a DLog equality proof binds the blinded and unblinded responses.

**(G4) Deterministic nullifier derivation.**

$$q = \mathrm{Poseidon2}(\text{"World ID Query"}, \; i, \; \mathrm{rpId}, \; a)$$

$$\nu = \mathrm{Poseidon2}(\text{"World ID Proof"}, \; q, \; R_{\mathrm{oprf}}.x, \; R_{\mathrm{oprf}}.y)$$

The nullifier $\nu$ is deterministic per $(i, \; \mathrm{rpId}, \; a)$ where $i$ is the leaf index and $a$ is the action. The same identity proving under the same $(\mathrm{rpId}, \; a)$ always yields the same $\nu$, regardless of blinding factor $\beta$ or proof randomness.

**(G5) Signal binding.** The `signalHash` is "dummy-squared" inside the circuit (constrained to $h \cdot h = h^2$ where $h$ is the signal hash), preventing the verifier from accepting a proof generated with a different signal.

**Soundness.** Under the knowledge soundness of Groth16 over $\mathbb{G}_{\mathrm{BN254}}$, an accepting proof guarantees the prover possesses a valid credential for a leaf in the committed tree, the OPRF evaluation was performed correctly under the registered key, and $\nu$ is deterministically bound to $(i, \; \mathrm{rpId}, \; a)$. Distinct action values yield distinct nullifier domains — a proof accepted under one action cannot be replayed under another.

---

#### 3.3.3 Inductive Nullifier Generations via Dual Proof Context Binding

Account creation requires **two** independent `OPRFNullifierProof`s from the same identity, each scoped to a distinct action domain.

Creation Proof $\pi_c$**

| Parameter    | Value                                    |
| ------------ | ---------------------------------------- |
| `action`     | `keccak256("WORLD_ID_ACCOUNT_CREATION")` |
| `signalHash` | `0`                                      |
| `rpId`       | `480`                                    |

Yields the **creation nullifier** $\nu_c$. This value is deterministic per-identity and independent of generation. It serves as the anchor binding the account generation to the respective `WORLD_ID_ACCOUNT_NONCE`.

Account Proof $\pi_a$**

Let $\eta = \mathrm{keccak256}(\nu_c \;\|\; \rho_c)$ be the **blinded generation index** (§3.5.6.1) where $\rho_c$ is a per-invocation blinding factor. Let $g = \mathtt{generations}[\eta]$ (current generation count read from state).

| Parameter    | Value                                                           |
| ------------ | --------------------------------------------------------------- |
| `action`     | `keccak256(abi.encodePacked(WORLD_CHAIN_RP_ID, uint256(0), g))` |
| `signalHash` | `keccak256(abi.encode(keys)) >> 8`                              |
| `rpId`       | `480`                                                           |

Yields the **account nullifier** $\nu_a$, unique per (identity, $g$). The `signalHash` commits to the authorized key set $K = \lbrace k_0, \ldots, k_{n-1} \rbrace$.

**Binding property.** Both proofs are verified against the same `WorldIDRegistry` root and `rpId = 480`. By the ZK property, the verifier learns only the nullifier hashes — no on-chain linkage between the two proofs' underlying identity is revealed.

---

#### 3.3.4 Account Public Key Derivation

The account address is a deterministic function of $\nu_a$:

$$\mathrm{addr}(\nu_a) = \mathrm{bytes20}(\mathrm{keccak256}(\mathrm{abi.encodePacked}(\nu_a)))$$

Since $\nu_a$ is bound to $(i, \; \mathrm{rpId}, \; a_g)$ and $a_g$ encodes $g$, each (identity, $g$) pair maps to exactly one address.

---

#### 3.3.5 Soundness

We define a process by which a `World ID Account` may choose to rotate their account via a proof which commits to its next generational nullifier. This recycle process grants another level of pseudo-anyonymity which allows a user to have the freedom from being anchored to a single account address. 

> 💡 The `generations` mapping is indexed by a **blinded key** $\eta = \mathrm{keccak256}(\nu_c \;\|\; \rho_c)$ rather than $\nu_c$ directly (§3.5.6.1). Each generation uses a fresh $\rho_c$, making mapping entries unlinkable by state scanning. At account recycling ($g > 0$), a **one-out-of-many proof** (§3.5.6.3) demonstrates the caller owned a prior generational commitment without revealing which one. These mechanisms achieve state-level and recycling-time unlinkability without circuit modifications. The sole remaining linkage vector is $\nu_c$ appearing as a public input to $\pi_c$ in calldata — eliminating this requires the circuit extension described in §3.5.6.4.

---

Fix a POH authenticator at leaf index $\ell$ with creation nullifier $\nu_c$. Let $g_t \in \mathbb{N}$ denote the generation counter at discrete time $t$. Let $\eta_t = \mathrm{keccak256}(\nu_c \;\|\; \rho_{c,t})$ denote the blinded generation index at time $t$ (§3.5.6.1). The protocol state after the $t$-th `createAccount` invocation is:

$$S_{g_t} = (\mathtt{generations}[\eta_t] = g_t, \;\; \mathcal{S}, \;\; \mathtt{spentAccountNullifiers}, \;\; \mathtt{destroyed})$$

where $\mathcal{S}$ is the generational commitment set (§3.5.6.2).

We claim the following invariants hold at every reachable state $S_{g_t}$:

**Lemma 1.1** (Monotonicity).

$$\forall\, t :\; g_{t+1} = g_t + 1 \;\wedge\; g_t \in \mathbb{N} \;\implies\; g_{t+1} \in \mathbb{N}$$

**Lemma 1.2** (Nullifier uniqueness).

$$\forall t : \; \mathtt{spentAccountNullifiers}[\nu_a(g_t)] = \mathsf{true} \implies \not\exists \; \pi' \; \text{s.t.} \; \mathrm{Verify}(\pi') \; \text{outputs} \; \nu_a(g_t)$$

**Lemma 1.3** (Address freshness).

$$g_t \neq g_{t'} \;\implies\; \mathrm{addr}(g_t) \neq \mathrm{addr}(g_{t'})$$

**Lemma 1.4** (Destruction finality).

$$\mathtt{destroyed}[\mathrm{addr}(g_t)] = \mathsf{true} \;\implies\; \mathrm{addr}(g_t)\;\text{is permanently excluded from initialization}$$

**Lemma 1.5** (Single-account-per-authenticator).

$$\forall \ell, \forall t : \; | \lbrace \mathrm{addr}(g_\tau) \mid \tau \leq t \wedge \mathtt{destroyed}[\mathrm{addr}(g_\tau)] = \mathsf{false} \rbrace | \leq 1$$

---

**Theorem 1.** *For any identity with creation nullifier $\nu_c$, the `createAccount` protocol satisfies **Lemmas 1.1–1.5** at every reachable state $S_{g_t}$.*

**Proof.** By induction on $t$.

Let $S_{g_0}$ be the initial state where $g_0 = 0$ at $t_0$.

$\mathtt{generations}[\nu_c] = 0$. No account has been created. The spent-set and destroyed-set are empty with respect to this identity:

$$\mathtt{spentAccountNullifiers}[\nu_a(g_0)] = \mathsf{false} \;\wedge\; \mathtt{destroyed}[\mathrm{addr}(g_0)] = \mathsf{false}$$

**Lemmas 1.1–1.5** hold vacuously $\;\square_0$

Assume **Lemmas 1.1–1.5** hold at state $S_{g_t}$ for some $t \geq 0$ with generation $g_t$.

We will show $(S_{g_t} \to S_{g_{t+1}})$. A `createAccount` call executes at time $t+1$ with proofs $\pi_c, \pi_a$. Step ② reads $g_t = \mathtt{generations}[\eta_t]$ via the blinded index, and step ⑩ writes $\mathtt{generations}[\eta_{t+1}] \leftarrow g_t + 1$ under a fresh blinded index $\eta_{t+1} = \mathrm{keccak256}(\nu_c \;\|\; \rho_{c,t+1})$, deleting the old entry $\mathtt{generations}[\eta_t]$. Define $g_{t+1} = g_t + 1$. We prove each lemma is preserved sequentially.

---

*Lemma 1.1.* Step ⑩ is the sole write site for the generation counter. It reads $g_t$ from $\mathtt{generations}[\eta_t]$ and writes $g_t + 1$ to $\mathtt{generations}[\eta_{t+1}]$ under a fresh blinded index, deleting the old entry. By the inductive hypothesis $g_t \in \mathbb{N}$, so $g_{t+1} = g_t + 1 \in \mathbb{N}$. The counter value is never decremented or reset; only the storage key changes. $\;\square$

---

*Lemma 1.2.* At time $t+1$, step ④ requires $\mathtt{spentAccountNullifiers}[\nu_a(g_t)] = \mathsf{false}$ and sets it to $\mathsf{true}$. Suppose for contradiction that at some future time $t' > t+1$, a proof $\pi'$ produces the same $\nu_a(g_t)$. By **(G4)**, $\nu_a$ is deterministic per $(\ell, \; \mathrm{rpId}, \; a)$. Since $a(g_t)$ is fixed for generation $g_t$, any valid $\pi'$ yielding $\nu_a(g_t)$ must use the same action — but the spent-set guard at step ④ rejects it. By **Lemma 1.1**, $g_{t'} > g_t$ for all $t' > t+1$, so the protocol cannot revisit generation $g_t$. $\;\square$

---

*Lemma 1.3.* By construction:

$$a(g_t) = \mathrm{keccak256}(\mathrm{abi.encodePacked}(\mathrm{rpId}, \; 0, \; g_t))$$

By **Lemma 1.1**, $t \neq t' \implies g_t \neq g_{t'}$. Since $\mathrm{keccak256}$ is collision-resistant and $g_t$ varies, $g_t \neq g_{t'}$ implies $a(g_t) \neq a(g_{t'})$. Distinct actions yield distinct OPRF queries $q(g_t) \neq q(g_{t'})$ by injectivity of $\mathrm{Poseidon2}$ over distinct inputs. By the pseudorandomness of the OPRF, $\nu_a(g_t) \neq \nu_a(g_{t'})$ with overwhelming probability. Finally, $\mathrm{addr}(\cdot)$ applies $\mathrm{keccak256}$, preserving distinctness by collision resistance. $\;\square$

---

*Lemma 1.4.* Step ⑤ requires $\mathtt{destroyed}[\mathrm{addr}(g_t)] = \mathsf{false}$ before initialization. The `destroyed` mapping is write-once (set to $\mathsf{true}$ on destruction, never reset). By **Lemma 1.3**, the successor state $S_{g_{t+1}}$ derives a fresh address $\mathrm{addr}(g_{t+1}) \neq \mathrm{addr}(g_t)$, so destruction at time $t$ does not obstruct $S_{g_{t+1}}$. $\;\square$

---

*Lemma 1.5.* The creation proof $\pi_c$ uses constants $a_c = \mathrm{keccak256}($`"WORLD_ID_ACCOUNT_CREATION"`$)$ and $\mathrm{rpId} = 480$. By **(G4)**, $\nu_c = f(\ell, \; \mathrm{rpId}, \; a_c)$ is a unique function of $\ell$. Two distinct authenticators $\ell \neq \ell'$ produce $\nu_c \neq \nu_c'$ by pseudorandomness of the OPRF.

At time $t+1$ with generation $g_t$, the account proof $\pi_a$ yields a deterministic $\nu_a(g_t)$ per $(\ell, g_t)$ by **(G4)**, and **Lemma 1.2** prevents $\nu_a(g_t)$ from being used twice. Therefore at most one $\mathrm{addr}(g_t)$ is created per $(\ell, g_t)$.

It remains to show that two different generations cannot both have live accounts simultaneously. Suppose $\mathrm{addr}(g_t)$ is live at time $t$. At time $t+1$, `createAccount` reads $g_t = \mathtt{generations}[\eta_t]$ at step ② and writes $g_{t+1} = g_t + 1$ to $\mathtt{generations}[\eta_{t+1}]$ at step ⑩, deleting $\mathtt{generations}[\eta_t]$. By **Lemma 1.1**, the counter strictly advances: $g_{t+1} > g_t$, and the old blinded index is deleted so no future invocation can read $g_t$ again. Therefore $a(g_t)$ is unreachable at any time $t' > t$, meaning $\mathrm{addr}(g_t)$ cannot be re-created. The new account $\mathrm{addr}(g_{t+1})$ is the sole live account for $\ell$. $\;\square$

---

**Lemmas 1.1–1.5** are preserved by the transition $S_{g_t} \to S_{g_{t+1}}$ and hold at $S_{g_0}$. By induction, they hold for all reachable states $S_{g_t}$ where $t \in \mathbb{N}$. $\;\blacksquare$

---

#### 3.3.6 Factory State

```solidity
contract WorldIDAccountFactory {

    // ─── State ─────────────────────────────────────────────────────────
    mapping(bytes32 => uint256) public generations;             // η (blinded index) → generation count (§3.5.6.1)
    mapping(uint256 => bool)    public spentAccountNullifiers;  // ν_a → spent
    mapping(address => bool)    public destroyed;               // addr → finalized

    // ─── Generational Commitment Set (§3.5.6.2) ───────────────────────
    uint256[2][] public commitmentSet;                          // S = {C_0^ν, C_1^ν, ...}  (G1 points)

    // ─── Types ─────────────────────────────────────────────────────────
    enum AuthType { Secp256k1, P256, WebAuthn, Ed25519 }

    struct AuthorizedKey {
        AuthType authType;
        bytes    keyData;   // 33 bytes (compressed secp256k1) or 64 bytes (P256/WebAuthn)
    }

    // ─── Events ────────────────────────────────────────────────────────
    event AccountCreated(address indexed account, uint256 generation);
    event AccountDestroyed(address indexed account, uint256 generation);
}
```

---

#### 3.3.7 `createAccount`

```solidity
function createAccount(
    uint256 creationNullifier,                  // ν_c (circuit output of π_c)
    uint256 accountNullifier,                   // ν_a (circuit output of π_a)
    uint256[5] calldata creationProof,          // compressed Groth16 proof π_c
    uint256[5] calldata accountProof,           // compressed Groth16 proof π_a
    uint64 expiresAtMin,
    uint64 issuerSchemaId,
    AuthorizedKey[] calldata keys,              // delegated key set K
    uint256 keyBlinding,                        // ρ for Pedersen key commitment C
    // ─── Blinded generation index (§3.5.6.1) ─────────────────────────
    uint256 blindingNew,                        // ρ_{c,new} — fresh blinding for new index
    uint256 blindingOld,                        // ρ_{c,old} — previous blinding (0 if g=0)
    // ─── Nullifier commitment (§3.5.6.2) ──────────────────────────────
    uint256[2] calldata nullifierCommitment,    // C_g^ν = ν_a · G_ν + ρ_ν · H_ν
    uint256 nullifierBlinding,                  // ρ_ν (verified then discarded)
    // ─── One-out-of-many proof (§3.5.6.3) ─────────────────────────────
    bytes calldata oomProof                     // π_oom (empty if g=0)
) external returns (address account) {

    require(keys.length > 0 && keys.length <= MAX_AUTHORIZED_KEYS);

    // ① Verify creation proof π_c
    IWorldIDVerifier(WORLD_ID_VERIFIER).verify(
        creationNullifier,
        uint256(keccak256("WORLD_ID_ACCOUNT_CREATION")),    // action_c
        WORLD_CHAIN_RP_ID,                                   // 480
        0,                                                   // nonce
        0,                                                   // signalHash
        expiresAtMin,
        issuerSchemaId,
        0,                                                   // credentialGenesisIssuedAtMin
        creationProof
    );

    // ② Read generation via blinded index (§3.5.6.1)
    bytes32 idxOld = keccak256(abi.encodePacked(creationNullifier, blindingOld));
    uint256 g = generations[idxOld];
    //   For g=0: idxOld is an unused slot (returns 0). blindingOld is ignored.
    //   For g>0: idxOld must map to the current generation count.

    // ③ Verify account proof π_a
    //    signalHash commits to the Pedersen key commitment (§3.5.4)
    bytes32 compressedC = _pedersenKeyCommitment(keys, keyBlinding);
    uint256 signalHash = uint256(keccak256(abi.encodePacked(compressedC))) >> 8;
    IWorldIDVerifier(WORLD_ID_VERIFIER).verify(
        accountNullifier,
        uint256(keccak256(abi.encodePacked(WORLD_CHAIN_RP_ID, uint256(0), g))),  // action_a(g)
        WORLD_CHAIN_RP_ID,
        0,
        signalHash,
        expiresAtMin,
        issuerSchemaId,
        0,
        accountProof
    );

    // ④ Nullifier uniqueness (Lemma 1.2)
    require(!spentAccountNullifiers[accountNullifier], "nullifier spent");
    spentAccountNullifiers[accountNullifier] = true;

    // ⑤ Verify nullifier commitment opening (§3.5.6.2)
    //    C_g^ν == ν_a · G_ν + ρ_ν · H_ν
    require(_verifyNullifierCommitment(
        nullifierCommitment, accountNullifier, nullifierBlinding
    ));

    // ⑥ If recycling (g > 0), verify one-out-of-many proof (§3.5.6.3)
    //    Proves the caller owned a prior commitment in S.
    if (g > 0) {
        require(oomProof.length > 0, "oom proof required for recycling");
        require(_verifyOomProof(commitmentSet, oomProof), "invalid oom proof");
        // Delete old blinded index entry
        delete generations[idxOld];
    }

    // ⑦ Derive address (§3.5.3)
    account = address(bytes20(keccak256(abi.encodePacked(accountNullifier, compressedC))));
    require(!destroyed[account], "account destroyed");          // (Lemma 1.4)

    // ⑧ Initialize account storage (§3.4)
    _initializeAccountStorage(account, keys, g, accountNullifier, compressedC);

    // ⑧b Epoch nonce carry (§3.6.5)
    if (g > 0) {
        _carryEpochNonce(oldAccount, account);  // internal precompile transfer
    } else {
        _initializeEpochNonce(account);          // fresh E = ρ_n · H_n, n = 0
    }

    // ⑨ Append nullifier commitment to set S (§3.5.6.2)
    commitmentSet.push(nullifierCommitment);

    // ⑩ Write new blinded index, advance generation (§3.5.6.1, Lemma 1.1)
    bytes32 idxNew = keccak256(abi.encodePacked(creationNullifier, blindingNew));
    generations[idxNew] = g + 1;

    emit AccountCreated(account, g);
}
```

---

### 3.4 Account Delegate Storage

Each World ID Account delegates its code via [EIP-7702](https://eips.ethereum.org/EIPS/eip-7702) to `WORLD_ID_ACCOUNT_DELEGATE`. Storage lives at the **account address** (not the delegate code address).

#### 3.4.1 Storage Layout

| Slot Name         | Key                                                   | Type            | Description                                  |
| ----------------- | ----------------------------------------------------- | --------------- | -------------------------------------------- |
| `GENERATION_SLOT` | `keccak256("worldchain.world_id_account.generation")` | `uint256`       | Generation `g`                               |
| `NUM_KEYS_SLOT`   | `keccak256("worldchain.world_id_account.num_keys")`   | `uint256`       | Key count `n`, `1 ≤ n ≤ MAX_AUTHORIZED_KEYS` |
| `KEY_SLOT(i)`     | `keccak256("worldchain.world_id_account.key" ‖ i)`    | `AuthorizedKey` | `i`-th key, `0 ≤ i < n`                      |
| `NULLIFIER_SLOT`  | `keccak256("worldchain.world_id_account.nullifier")`  | `uint256`       | Account nullifier `ν_a`                      |
| `COMMITMENT_SLOT` | `keccak256("worldchain.world_id_account.commitment")` | `bytes32`       | Compressed Pedersen key commitment `compress(C)` |
| `NULL_COMMIT_SLOT`| `keccak256("worldchain.world_id_account.null_commitment")` | `bytes32` | Compressed nullifier commitment `compress(C_g^ν)` (§3.5.6.2) |
| `EPOCH_NONCE_SLOT`| `keccak256("worldchain.world_id_account.epoch_nonce")` | `bytes32` | Compressed epoch nonce commitment `compress(E)` (§3.6)    |
| `EPOCH_NONCE_VALUE_SLOT`| `keccak256("worldchain.world_id_account.epoch_nonce_value")` | `uint256` | Plaintext epoch nonce `n` (for consensus-layer rate check) |
| `EPOCH_NUMBER_SLOT`| `keccak256("worldchain.world_id_account.epoch_number")` | `uint256` | Last-seen epoch number `ε`                               |

#### 3.4.2 Key Encoding

Each `AuthorizedKey` occupies two sequential slots at `KEY_SLOT(i)`:

```
slot[KEY_SLOT(i)]     = uint256(authType) | (keyData.length << 8)
slot[KEY_SLOT(i) + 1] = bytes32(keyData)
```

| `AuthType` | Value  | `keyData` | Description                                                                      |
| ---------- | ------ | --------- | -------------------------------------------------------------------------------- |
| Secp256k1  | `0x00` | 33 bytes  | Compressed public key                                                            |
| P256       | `0x01` | 64 bytes  | Uncompressed `(x, y)`                                                            |
| WebAuthn   | `0x02` | 64 bytes  | Uncompressed `(x, y)` (P256 under [WebAuthn](https://www.w3.org/TR/webauthn-3/)) |
| Ed25519    | `0x03` | 32 bytes  | Ed25519 verifying key                                                            |

#### 3.4.3 Initialization Sequence

On `createAccount`, the factory writes:

```
account.storage[GENERATION_SLOT]       ← g
account.storage[NUM_KEYS_SLOT]         ← |K|
account.storage[KEY_SLOT(i)]           ← encode(kᵢ)    ∀ i ∈ [0, |K|)
account.storage[NULLIFIER_SLOT]        ← ν_a
account.storage[COMMITMENT_SLOT]       ← compress(C)
account.storage[EPOCH_NONCE_SLOT]      ← compress(E)    // carried or fresh (§3.6.5)
account.storage[EPOCH_NONCE_VALUE_SLOT]← n               // carried or 0
account.storage[EPOCH_NUMBER_SLOT]     ← ε               // current epoch
account.code                      ← 0xef0100 ‖ WORLD_ID_ACCOUNT_DELEGATE
```

#### 3.4.4 Signal Binding Invariant

The key set $K$ written to storage is **exactly** the set committed in $\pi_a$ via the Pedersen commitment $C$:

$$h_a = \mathrm{keccak256}(\mathrm{compress}(C)) \gg 8$$

where $C = \sum_{i=0}^{m-1} h_i \cdot G_i + \rho \cdot B$ is the blinded Pedersen vector commitment over the key hashes.

By **(G5)**, the proof is bound to this signal hash $h_a$. Any post-proof modification to $K$ or $\rho$ would change $C$, which changes $h_a$, which invalidates the proof. Key rotation therefore requires account recycling: $S_g \to S_{g+1}$.

---

### 3.5 Blinded Pedersen Vector Commitment for Key Authorization

The authorized key set is committed via a **blinded Pedersen vector commitment** over [BN254](https://hackmd.io/@jpw/bn254). This construction achieves three properties simultaneously:

1. **Stateless O(1) key authorization** — a transaction signature is verifiable against the account address with no on-chain storage lookups
2. **Key set privacy** — the commitment hides the number and identity of authorized keys
3. **Cross-generation unlinkability** — successive account generations produce addresses with no observable link

#### 3.5.1 Generator Points

Let $\mathbb{G}_1$ denote the BN254 $G_1$ group of prime order $r$. Let $n = \mathtt{MAX\_AUTHORIZED\_KEYS} = 20$ and $N = \mathtt{PEDERSEN\_N} = 2^{\lceil \log_2 n \rceil} = 32$. Define:

- **Commitment generators**: $\mathbf{G} = (G_0, \; G_1, \; \ldots, \; G_{N-1}) \;\in\; \mathbb{G}_1^{N}$
- **IPA auxiliary generators**: $\mathbf{H} = (H_0, \; H_1, \; \ldots, \; H_{N-1}) \;\in\; \mathbb{G}_1^{N}$
- **Blinding generator**: $B \;\in\; \mathbb{G}_1$
- **Inner product binding generator**: $Q \;\in\; \mathbb{G}_1$

All $2N + 2 = 66$ points are derived deterministically via hash-to-curve — **no trusted setup** is required:

```
G_i = HashToCurve("WorldChain.Pedersen.G" || i)    for i ∈ [0, N)
H_i = HashToCurve("WorldChain.Pedersen.H" || i)    for i ∈ [0, N)
B   = HashToCurve("WorldChain.Pedersen.B")
Q   = HashToCurve("WorldChain.Pedersen.Q")
```

Because each point is derived independently via a cryptographic hash, the discrete logarithm relationship between any pair is unknown. This ensures the commitment is **binding** (the committer cannot find two different openings that produce the same commitment point) and that the Bulletproofs IPA is sound.

#### 3.5.2 Key Commitment Construction

Given an authorized key set $K = \lbrace k_0, \ldots, k_{m-1} \rbrace$ where $m \leq n$, compute key hashes:

$$h_i = \mathrm{keccak256}(\mathtt{authType}_i \;\|\; \mathtt{keyData}_i) \mod r$$

Define the **padded key hash vector** $\mathbf{v} \in \mathbb{F}_r^{N}$:

$$v_i = \begin{cases} h_i & \text{if } i < m \\ 0 & \text{if } m \leq i < N \end{cases}$$

The **blinded Pedersen vector commitment** is:

$$C = \langle \mathbf{v}, \; \mathbf{G} \rangle + \rho \cdot B = \sum_{i=0}^{m-1} h_i \cdot G_i + \rho \cdot B$$

where $\rho \xleftarrow{\$} \mathbb{F}_r$ is a uniformly random **blinding factor** and the zero-padded terms vanish from the sum.

**Properties:**

- **Hiding.** The blinding factor $\rho$ makes $C$ computationally indistinguishable from a uniformly random point in $\mathbb{G}_1$, regardless of the key hashes $h_i$. An observer learns nothing about the key set from $C$ alone.

- **Binding.** Under the discrete logarithm assumption over $\mathbb{G}_1$, the committer cannot find $(\mathbf{v}', \rho')$ with $\mathbf{v}' \neq \mathbf{v}$ such that $\langle \mathbf{v}', \mathbf{G} \rangle + \rho' B = C$. The committed key set is immutable once $C$ is published.

- **Additively homomorphic.** Given two commitments $C_1 = \langle \mathbf{v}_1, \mathbf{G} \rangle + \rho_1 B$ and $C_2 = \langle \mathbf{v}_2, \mathbf{G} \rangle + \rho_2 B$, their sum $C_1 + C_2$ is a valid commitment to $\mathbf{v}_1 + \mathbf{v}_2$ with blinding factor $\rho_1 + \rho_2$.

#### 3.5.3 Account Address Derivation (Updated)

The account address now embeds the Pedersen commitment:

$$\mathrm{addr}(\nu_a, C) = \mathrm{bytes20}\big(\mathrm{keccak256}(\nu_a \;\|\; \mathrm{compress}(C))\big)$$

where $\mathrm{compress}(C)$ is the 32-byte compressed representation of the BN254 $G_1$ point $C$.

This replaces the prior derivation $\mathrm{addr}(\nu_a) = \mathrm{bytes20}(\mathrm{keccak256}(\nu_a))$. The address itself now **commits to the authorized key set** — any modification to the key set changes the address.

#### 3.5.4 Signal Hash (Updated)

The account proof $\pi_a$'s signal hash commits to the Pedersen commitment rather than the raw key encoding:

$$h_a = \mathrm{keccak256}(\mathrm{compress}(C)) \gg 8$$

By **(G5)**, the OPRF proof is bound to this signal, which in turn is bound to the specific key set and blinding factor. Substituting a different key set or blinding factor invalidates the proof.

#### 3.5.5 Transaction Signature Verification (Stateless)

A `0x6f` transaction carries the compressed commitment point $C$, a [Bulletproofs](https://eprint.iacr.org/2017/1066) **inner product argument** $\pi_{\mathrm{ipa}}$, and the cryptographic signature. Verification proceeds without any on-chain state access:

**Step 1 — Address binding.** Verify the commitment matches the sender address:

$$\mathrm{addr}(\nu_a, C) \stackrel{?}{=} \mathtt{tx.sender}$$

If the commitment point were tampered with, the derived address would not match. The commitment is **self-certifying**.

**Step 2 — Signature verification.** Verify the cryptographic signature over the signing hash. For secp256k1, recover the signer's compressed public key via `ecrecover`. For P256, WebAuthn, and Ed25519, the signer's public key is carried in the signature payload.

**Step 3 — Key membership via Bulletproofs inner product argument.** Compute the signer's key hash:

$$h_{\mathrm{signer}} = \mathrm{keccak256}(\mathtt{authType} \;\|\; \mathtt{keyData}) \mod r$$

The prover constructs an IPA instance over the padded key hash vector $\mathbf{v} \in \mathbb{F}_r^N$ (§3.5.2) and the public unit selector $\mathbf{e}_j \in \mathbb{F}_r^N$ (with $1$ at the signer's index $j$ and $0$ elsewhere), asserting:

$$\langle \mathbf{v}, \; \mathbf{e}_j \rangle = h_{\mathrm{signer}}$$

The verifier reconstructs the IPA commitment $P$ from public values. Because $\rho$ is absorbed into the witness at a padding position with $G_p := B$ (see **Blinding Factor Handling** below), the Pedersen commitment $C$ already encodes the blinding factor within the IPA generator structure:

$$P = C + H_j + h_{\mathrm{signer}} \cdot Q$$

No knowledge of $\rho$ is required — the commitment is self-contained.

**Bulletproofs IPA Protocol** (Bünz et al., Protocol 2). The proof operates on vectors of length $N$ via $k = \log_2 N = 5$ recursive halving rounds. In each round $\ell = 1, \ldots, k$:

1. The prover computes cross-commitment terms:

$$L_\ell = \langle \mathbf{a}_{L}, \; \mathbf{G}_{R} \rangle + \langle \mathbf{b}_{R}, \; \mathbf{H}_{L} \rangle + \langle \mathbf{a}_{L}, \; \mathbf{b}_{R} \rangle \cdot Q$$

$$R_\ell = \langle \mathbf{a}_{R}, \; \mathbf{G}_{L} \rangle + \langle \mathbf{b}_{L}, \; \mathbf{H}_{R} \rangle + \langle \mathbf{a}_{R}, \; \mathbf{b}_{L} \rangle \cdot Q$$

where subscripts $L, R$ denote the left/right halves of the current vectors.

2. A challenge $x_\ell \in \mathbb{F}_r$ is derived via Fiat-Shamir from the transcript $(L_\ell, R_\ell)$.

3. Vectors are folded: $\mathbf{a}' = x_\ell \mathbf{a}_L + x_\ell^{-1} \mathbf{a}_R$, $\;\mathbf{b}' = x_\ell^{-1} \mathbf{b}_L + x_\ell \mathbf{b}_R$, and generators $\mathbf{G}' = x_\ell^{-1} \mathbf{G}_L + x_\ell \mathbf{G}_R$, $\;\mathbf{H}' = x_\ell \mathbf{H}_L + x_\ell^{-1} \mathbf{H}_R$.

After $k$ rounds, the prover sends the final scalars $(a, b) \in \mathbb{F}_r^2$.

**Proof structure:**

$$\pi_{\mathrm{ipa}} = \big(L_1, R_1, \; \ldots, \; L_k, R_k, \; a, \; b\big)$$

**Proof size:** $2k$ compressed $\mathbb{G}_1$ points + $2$ field elements $= 2 \cdot 5 \cdot 32 + 2 \cdot 32 = 384$ bytes.

**Verification.** The verifier computes the folded generators $G', H'$ via the $k$ challenges and checks:

$$\sum_{\ell=1}^{k} \big(x_\ell^2 \cdot L_\ell + x_\ell^{-2} \cdot R_\ell\big) + a \cdot G' + b \cdot H' + a \cdot b \cdot Q \stackrel{?}{=} P$$

This reduces to a single multi-scalar multiplication, verifiable via the BN254 [ecAdd](https://eips.ethereum.org/EIPS/eip-196) (`0x06`) and [ecMul](https://eips.ethereum.org/EIPS/eip-196) (`0x07`) precompiles.

**Blinding factor handling.** The blinding factor $\rho$ is incorporated by assigning it to a designated padding position $p \in [m, N)$ in $\mathbf{v}$ and setting $G_p := B$. The selector $\mathbf{e}_j$ has $e_p = 0$, so the blinding factor does not contribute to the inner product $\langle \mathbf{v}, \mathbf{e}_j \rangle$ and the proof remains zero-knowledge with respect to both $\rho$ and the uncommitted key hashes.

**Verification cost.** $O(N)$ group operations dominated by the multi-scalar multiplication for generator folding, with $k = 5$ rounds of Fiat-Shamir challenges. For $N = 32$, this is concretely $\leq 70$ [ecMul](https://eips.ethereum.org/EIPS/eip-196) and [ecAdd](https://eips.ethereum.org/EIPS/eip-196) calls.

#### 3.5.6 Cross-Generation Unlinkability

Under the original scheme (§3.3.3), the creation nullifier $\nu_c$ appears as a public input in every `createAccount` call and serves as the key into the `generations` mapping, linking all generations of the same identity at both the calldata and state level.

This section introduces two complementary mechanisms — a **blinded generation index** and a **one-out-of-many proof** — that achieve state-level unlinkability and recycling-time privacy without circuit modifications. Both operate entirely within BN254 $\mathbb{G}_1$ using the same hash-derived generators as the key commitment scheme. No trusted setup is required.

##### 3.5.6.1 Blinded Generation Index

The `generations` mapping is re-keyed from $\nu_c$ to a **blinded index** $\eta$:

$$\eta = \mathrm{keccak256}(\nu_c \;\|\; \rho_c)$$

where $\rho_c \xleftarrow{\$} \mathbb{F}_r$ is a per-invocation blinding factor chosen by the caller. Each `createAccount` invocation uses a fresh $\rho_c$, producing a pseudorandom-looking mapping key. The precompile processes $\nu_c$ and $\rho_c$ internally; the mapping key $\eta$ is the sole on-chain state artifact.

**Index lifecycle.** At generation $g_t$, the mapping contains a single entry $\mathtt{generations}[\eta_t] = g_t$. When the account recycles to generation $g_{t+1}$:

1. The caller provides the old blinding factor $\rho_{c,t}$ (to locate the current entry) and a fresh $\rho_{c,t+1}$.
2. The precompile computes $\eta_t = \mathrm{keccak256}(\nu_c \;\|\; \rho_{c,t})$ and reads $g_t = \mathtt{generations}[\eta_t]$.
3. The precompile computes $\eta_{t+1} = \mathrm{keccak256}(\nu_c \;\|\; \rho_{c,t+1})$ and writes $\mathtt{generations}[\eta_{t+1}] = g_t + 1$.
4. The old entry $\mathtt{generations}[\eta_t]$ is deleted.

Because $\eta_t$ and $\eta_{t+1}$ are outputs of $\mathrm{keccak256}$ over distinct inputs, they are computationally unlinkable without knowledge of $\nu_c$. An observer scanning the `generations` mapping sees pseudorandom keys with no structural relationship.

**Correctness verification.** The precompile verifies the caller's $\rho_{c,t}$ is consistent with $\nu_c$ (obtained from $\pi_c$ verification) by recomputing $\eta_t$ and checking that $\mathtt{generations}[\eta_t]$ exists. No additional proof is needed — the precompile has $\nu_c$ as a verified public input from $\pi_c$.

##### 3.5.6.2 Generational Nullifier Commitments

Each `createAccount` invocation publishes a [Pedersen commitment](https://link.springer.com/chapter/10.1007/3-540-46766-1_9) to the account nullifier:

$$C_g^{\nu} = \nu_a(g) \cdot G_{\nu} + \rho_g^{\nu} \cdot H_{\nu}$$

where $G_{\nu}, H_{\nu} \in \mathbb{G}_1$ are independent hash-derived generators (see Constants) and $\rho_g^{\nu} \xleftarrow{\$} \mathbb{F}_r$ is a uniformly random blinding factor. The on-chain **commitment set** $\mathcal{S}$ accumulates one commitment per account creation:

$$\mathcal{S} = \lbrace C_0^{\nu}, \; C_1^{\nu}, \; \ldots \rbrace$$

**Properties.**

- **Hiding.** $C_g^{\nu}$ is computationally indistinguishable from a uniformly random $\mathbb{G}_1$ point. An observer learns nothing about $\nu_a(g)$ from $C_g^{\nu}$ alone.

- **Binding.** Under the discrete logarithm assumption over $\mathbb{G}_1$, the committer cannot open $C_g^{\nu}$ to a different nullifier.

- **Opening verification.** At `createAccount` time, the precompile verifies the commitment is well-formed by checking $C_g^{\nu} = \nu_a(g) \cdot G_{\nu} + \rho_g^{\nu} \cdot H_{\nu}$ using the verified $\nu_a(g)$ from $\pi_a$ and the caller-provided $\rho_g^{\nu}$. After verification, $\rho_g^{\nu}$ is discarded — it exists only in the wallet.

##### 3.5.6.3 One-Out-of-Many Proof for Account Recycling

When an account recycles from generation $g$ to $g+1$, the caller must prove they owned a prior account in $\mathcal{S}$ without revealing which one. This is achieved via a [Groth-Kohlweiss one-out-of-many proof](https://eprint.iacr.org/2014/764) — a sigma protocol compiled via [Fiat-Shamir](https://link.springer.com/chapter/10.1007/3-540-47721-7_12), operating entirely within BN254 $\mathbb{G}_1$.

**Statement.** The prover demonstrates:

> *"I know an index $\ell \in [0, |\mathcal{S}|)$ and an opening $(\nu_a(\ell), \rho_\ell^{\nu})$ such that $C_\ell^{\nu} \in \mathcal{S}$."*

without revealing $\ell$, $\nu_a(\ell)$, or $\rho_\ell^{\nu}$.

**Protocol.** Let $N = |\mathcal{S}|$ and $n = \lceil \log_2 N \rceil$. Pad $\mathcal{S}$ to $2^n$ elements with commitments to zero.

1. **Bit commitments.** Decompose $\ell = \sum_{k=0}^{n-1} \ell_k \cdot 2^k$ where $\ell_k \in \lbrace 0, 1 \rbrace$. For each bit $k$, commit:

$$B_k = \ell_k \cdot G_{\nu} + s_k \cdot H_{\nu}$$

where $s_k \xleftarrow{\$} \mathbb{F}_r$. These commitments are proven to be well-formed bits via a standard Schnorr proof of $\ell_k(\ell_k - 1) = 0$.

2. **Selector polynomials.** For each $j \in [0, 2^n)$, define:

$$p_j(x) = \prod_{k=0}^{n-1} \bigl(\delta_{j,k} + f_{j,k} \cdot x\bigr)$$

where $\delta_{j,k} = \ell_k$ if $j_k = 1$ else $1 - \ell_k$, and $f_{j,k}$ are Fiat-Shamir-derived masking scalars. By construction, $p_j(0) = [\,j = \ell\,]$.

3. **Cross-term commitments.** For each degree $d = 1, \ldots, n$:

$$D_d = \sum_{j=0}^{N-1} p_j^{(d)} \cdot C_j^{\nu} + \rho_d \cdot H_{\nu}$$

where $p_j^{(d)}$ is the degree-$d$ coefficient of $p_j(x)$ and $\rho_d \xleftarrow{\$} \mathbb{F}_r$.

4. **Challenge.** $x \leftarrow \mathrm{Hash}(\mathcal{S}, B_0, \ldots, B_{n-1}, D_1, \ldots, D_n)$

5. **Responses.** For each bit $k$: $z_k = \ell_k \cdot x + s_k$ (mod $r$). Blinding response: $z_\rho = \rho_\ell^{\nu} \cdot x^n + \sum_{d=1}^{n} \rho_d \cdot x^{n-d}$ (mod $r$).

**Verification.** The verifier computes the selector values $p_j(x)$ from $\lbrace z_k \rbrace$ and checks:

$$\sum_{j=0}^{N-1} p_j(x) \cdot C_j^{\nu} + z_\rho \cdot H_{\nu} \stackrel{?}{=} \sum_{d=0}^{n} x^{n-d} \cdot D_d$$

where $D_0$ is reconstructed from the bit commitments $\lbrace B_k \rbrace$ and the challenge $x$. This reduces to a single multi-scalar multiplication of size $N + n + 1$.

**Proof structure.**

$$\pi_{\mathrm{oom}} = \bigl(B_0, \ldots, B_{n-1}, \; D_1, \ldots, D_n, \; z_0, \ldots, z_{n-1}, \; z_\rho\bigr)$$

**Proof size.** $2n$ compressed $\mathbb{G}_1$ points + $(n + 1)$ field elements. For $N = 256$ ($n = 8$): $16 \times 32 + 9 \times 32 = 800$ bytes.

**Verification cost.** Dominated by an MSM of size $N + n + 1$. For $N = 256$: $\approx 265$ [ecMul](https://eips.ethereum.org/EIPS/eip-196) + [ecAdd](https://eips.ethereum.org/EIPS/eip-196) calls. Within the stateful precompile, this executes natively via `ark-bn254` at $\approx 5\text{ms}$.

**Soundness.** Under the discrete logarithm assumption over $\mathbb{G}_1$, extracting the witness $(\ell, \nu_a(\ell), \rho_\ell^{\nu})$ from an accepting proof is computationally infeasible without knowing the opening. The proof is **zero-knowledge**: the verifier's view is simulatable given only $|\mathcal{S}|$.

**When required.** The one-out-of-many proof is required only for account recycling ($g > 0$). First-time account creation ($g = 0$) has no prior generation to link to and does not submit $\pi_{\mathrm{oom}}$.

##### 3.5.6.4 Privacy Analysis

| Layer | Observable | Private |
| ----- | ---------- | ------- |
| **Calldata** (createAccount) | $\nu_c$ (public input to $\pi_c$), $\nu_a(g)$ (public input to $\pi_a$) | $\rho_c$, $\rho_g^{\nu}$, blinding factors |
| **State** (generations mapping) | Pseudorandom keys $\eta_t$ | Linkage between mapping entries |
| **State** (commitment set $\mathcal{S}$) | Hiding commitments $C_g^{\nu}$ | Which commitment belongs to which identity |
| **Recycling** (createAccount $g > 0$) | A valid prior commitment exists | Which commitment was recycled |
| **Transaction** (0x6f) | Sender address, signature, key commitment | Generation index, nullifier, other keys |

**Calldata linkage.** The creation nullifier $\nu_c$ remains a public input to $\pi_c$ (required for Groth16 verification without circuit changes). Two `createAccount` transactions from the same identity sharing $\nu_c$ in their calldata are linkable by an observer monitoring transaction submission. This is the sole remaining linkage vector.

**State unlinkability.** The blinded generation index $\eta_t$ and the hiding commitment $C_g^{\nu}$ ensure that scanning on-chain state reveals no identity groupings. The `generations` mapping contains pseudorandom keys; the commitment set $\mathcal{S}$ contains uniformly random-looking points.

**Recycling privacy.** The one-out-of-many proof ensures that recycling reveals only that the caller owned *some* prior commitment in $\mathcal{S}$, not which one. The anonymity set is $|\mathcal{S}|$ — every World ID Account ever created.

**Transaction privacy.** The `0x6f` transaction envelope (§3.7) contains no OOM proof, no blinding factors, and no generation index. The address is self-certifying via $\mathrm{addr}(\nu_a, C)$ and the IPA proves key membership. The account's generational history is invisible at transaction time.

> **Note.** Full calldata-level unlinkability — hiding $\nu_c$ from `createAccount` submissions — requires the creation proof $\pi_c$ to witness $\nu_c$ privately inside the Groth16 circuit. This is architecturally compatible with the existing circuit but requires an additional constraint set. The blinded index and OOM scheme described here are independent of — and can be deployed before — that circuit extension. When the circuit extension is deployed, $\nu_c$ moves from public input to private witness, and the calldata linkage row in the table above is eliminated.

---

### 3.6 Epoch Nonce Commitment

A World ID Account requires a **monotonic transaction counter** per identity that (a) survives account recycling, (b) supports privacy-preserving rate limiting, and (c) composes with the existing Pedersen commitment infrastructure. This section defines an **epoch nonce commitment** — a Pedersen commitment to the cumulative transaction count within a time-bounded epoch — managed by the consensus layer and carried across generations by the factory precompile.

#### 3.6.1 Motivation

World Chain sponsors gas for verified humans up to a per-epoch threshold. The epoch nonce provides the protocol-level primitive for this:

- **Rate limiting.** Reject `0x6f` transactions when the identity's epoch count reaches the threshold.
- **Privacy.** The committed nonce hides the exact transaction count from external observers (though the count per *address* is observable from the chain; the commitment's value is in cross-generation carry and algebraic composability).
- **Identity continuity.** The nonce is per-identity, not per-address. Recycling the account does not reset the epoch counter.

#### 3.6.2 Epoch Definition

An **epoch** $\varepsilon$ is a contiguous range of $\mathtt{EPOCH\_LENGTH}$ L2 blocks:

$$\varepsilon = \left\lfloor \frac{\mathtt{block.number}}{\mathtt{EPOCH\_LENGTH}} \right\rfloor$$

| Constant | Value | Description |
| -------- | ----- | ----------- |
| `EPOCH_LENGTH` | `43200` | Blocks per epoch (~1 day at 2s block time) |
| `EPOCH_TX_LIMIT` | `300` | Maximum sponsored transactions per epoch |
| `NONCE_G` | `HashToCurve("WorldChain.EpochNonce.G")` | BN254 G₁ generator for nonce value |
| `NONCE_H` | `HashToCurve("WorldChain.EpochNonce.H")` | BN254 G₁ blinding generator for nonce |

#### 3.6.3 Nonce Commitment Construction

The **epoch nonce commitment** is a Pedersen commitment to the transaction count $n$ within the current epoch:

$$E = n \cdot G_n + \rho_n \cdot H_n$$

where $G_n, H_n \in \mathbb{G}_1$ are independent hash-derived generators (see §3.6.2) and $\rho_n \in \mathbb{F}_r$ is the current blinding factor.

**Properties.**

- **Hiding.** $E$ is computationally indistinguishable from a uniformly random $\mathbb{G}_1$ point. The nonce value $n$ is not extractable from $E$ alone.

- **Binding.** Under the discrete logarithm assumption, $E$ can only be opened to $(n, \rho_n)$. The consensus layer cannot forge a different count.

- **Homomorphic increment.** Adding one transaction:

$$E' = E + G_n + r \cdot H_n = (n + 1) \cdot G_n + (\rho_n + r) \cdot H_n$$

where $r \xleftarrow{\$} \mathbb{F}_r$ rerandomizes the blinding. This is two group operations — one scalar multiplication and one addition.

#### 3.6.4 Consensus-Layer Nonce Management

The epoch nonce commitment is **managed entirely by the consensus layer** — the `0x6f` transaction envelope carries no nonce proof. This preserves the lightweight transaction format (§3.7).

**Per-transaction processing.** When the node validates a `0x6f` transaction:

1. Read $E$ and the plaintext nonce $n$ from account storage at `EPOCH_NONCE_SLOT` and `EPOCH_NONCE_VALUE_SLOT`.
2. Read the stored epoch number $\varepsilon_{\mathrm{stored}}$ from `EPOCH_NUMBER_SLOT`.
3. Compute current epoch $\varepsilon_{\mathrm{current}} = \lfloor \mathtt{block.number} / \mathtt{EPOCH\_LENGTH} \rfloor$.
4. **If $\varepsilon_{\mathrm{current}} > \varepsilon_{\mathrm{stored}}$** — epoch boundary crossed:
   - Reset: $n \leftarrow 0$, $\rho_n \xleftarrow{\$} \mathbb{F}_r$, $E \leftarrow \rho_n \cdot H_n$.
   - Write $\varepsilon_{\mathrm{current}}$ to `EPOCH_NUMBER_SLOT`.
5. **Rate check.** If $n \geq \mathtt{EPOCH\_TX\_LIMIT}$, the transaction is ineligible for gas sponsorship. (The transaction may still be valid — it simply pays its own gas.)
6. **Increment.** $n \leftarrow n + 1$. Pick $r \xleftarrow{\$} \mathbb{F}_r$. Compute $E' = E + G_n + r \cdot H_n$.
7. Write $(E', n, \varepsilon_{\mathrm{current}})$ to storage.

The plaintext nonce $n$ at `EPOCH_NONCE_VALUE_SLOT` is stored for the consensus layer's rate check (step 5). The commitment $E$ at `EPOCH_NONCE_SLOT` provides the binding guarantee and enables external range proofs.

**No per-transaction proof.** The consensus layer is trusted to perform the increment correctly. Correctness is enforced by the deterministic state transition function — any node re-executing the block can verify the nonce evolution. The commitment provides **auditability**: an external verifier can check that the sequence of $E$ values is consistent with the claimed increments via the relation $E_{i+1} - E_i - G_n \in \langle H_n \rangle$ (i.e., the difference is a scalar multiple of the blinding generator).

#### 3.6.5 Cross-Generation Nonce Carry

When an account recycles from generation $g$ to $g+1$, the epoch nonce must not reset — otherwise a user could evade rate limits by recycling. The factory precompile transfers the nonce internally:

**At `createAccount` (recycling, $g > 0$):**

1. The precompile knows the old account's address from the blinded generation index $\eta_t$ (§3.5.6.1).
2. Read $(E_{\mathrm{old}}, n_{\mathrm{old}}, \varepsilon_{\mathrm{old}})$ from the old account's storage.
3. If $\varepsilon_{\mathrm{old}} < \varepsilon_{\mathrm{current}}$: the epoch has rolled since the old account was last active. Reset: $n \leftarrow 0$, fresh $\rho_n$, $E \leftarrow \rho_n \cdot H_n$.
4. Otherwise: carry forward. Rerandomize $E_{\mathrm{old}}$ with fresh blinding:

$$E_{\mathrm{new}} = E_{\mathrm{old}} + r_{\mathrm{carry}} \cdot H_n = n_{\mathrm{old}} \cdot G_n + (\rho_{\mathrm{old}} + r_{\mathrm{carry}}) \cdot H_n$$

5. Write $(E_{\mathrm{new}}, n_{\mathrm{old}}, \varepsilon_{\mathrm{current}})$ to the new account's storage.

This is an **internal precompile operation** — the old address is known to the precompile from the blinded index but is not revealed in calldata or events. The nonce carry creates no external linkage between old and new accounts.

#### 3.6.6 Composite State Commitment

The account's full committed state is the pair $(C, E)$ where $C$ is the key commitment (§3.5.2) and $E$ is the epoch nonce commitment. The **composite state commitment** is defined as:

$$\Sigma = C + E = \left(\sum_{i=0}^{m-1} h_i \cdot G_i + \rho \cdot B\right) + \left(n \cdot G_n + \rho_n \cdot H_n\right)$$

Because $G_n, H_n$ are independent of the key commitment generators $\mathbf{G}, B$ (all derived via distinct `HashToCurve` tags), $\Sigma$ is a valid Pedersen vector commitment over the extended vector $(\mathbf{v} \;\|\; n)$ with independent blinding terms. The additive homomorphism ensures:

- **Key membership IPA** still operates against $C$ (extract $C = \Sigma - E$, where $E$ is read from storage).
- **Range proofs** operate against $E$ (extract $E = \Sigma - C$, where $C$ is from the address derivation).
- **Joint proofs** can operate directly on $\Sigma$ — e.g., proving "I hold a valid key AND my epoch nonce is below 300" in a single composed proof using the same Bulletproofs machinery.

#### 3.6.7 Optional Range Proof for External Verification

For scenarios where an external party (not the consensus layer) must verify the epoch nonce — e.g., a smart contract gating sponsored execution, or a cross-chain bridge verifying rate compliance — a **Bulletproofs range proof** over $E$ proves $n < 2^k$ without revealing $n$:

$$\pi_{\mathrm{range}} : \exists\, (n, \rho_n) \;\text{s.t.}\; E = n \cdot G_n + \rho_n \cdot H_n \;\wedge\; n \in [0, 2^k)$$

where $k = \lceil \log_2(\mathtt{EPOCH\_TX\_LIMIT}) \rceil = 9$ (for limit 300, prove $n < 512$).

**Proof size.** $2 \lceil \log_2 k \rceil + 2 = 2 \cdot 4 + 2 = 10$ group elements + $2$ field elements $= 10 \cdot 32 + 2 \cdot 32 = 384$ bytes.

**Verification.** $O(\log k) = 4$ rounds of Fiat-Shamir challenges, each involving a constant number of [ecMul](https://eips.ethereum.org/EIPS/eip-196) and [ecAdd](https://eips.ethereum.org/EIPS/eip-196) calls. Concretely $\leq 40$ group operations.

This proof is **not part of the `0x6f` transaction**. It is generated on-demand by the wallet and submitted to whatever external verifier requires it.

---

### 3.7 Transaction Type `0x6f`

#### 3.7.1 Envelope

```
0x6f || rlp([
    chain_id,                    // 0
    nonce,                       // 1
    max_priority_fee_per_gas,    // 2
    max_fee_per_gas,             // 3
    gas_limit,                   // 4
    to,                          // 5
    value,                       // 6
    data,                        // 7
    access_list,                 // 8
    authorization_list,          // 9
    account_nullifier,           // 10
    key_commitment,              // 11  ← compressed BN254 G₁ point (32 bytes)
    signature_type,              // 12  ← signature
    signature_payload,           // 13
    ipa_proof,                   // 14  ← Bulletproofs IPA (key membership, 384 bytes)
])
```

```
signing_hash = keccak256(0x6f || rlp(fields[0..=10]))
```

| `signature_type`                                          | `signature_payload`                                          |
| --------------------------------------------------------- | ------------------------------------------------------------ |
| `0x00` [Secp256k1](https://en.bitcoin.it/wiki/Secp256k1)  | `rlp([y_parity, r, s])`                                     |
| `0x01` [P256](https://neuromancer.sk/std/nist/P-256)      | `rlp([key, r, s])`                                          |
| `0x02` [WebAuthn](https://www.w3.org/TR/webauthn-3/)      | `rlp([key, authenticator_data, client_data_json, r, s])`    |
| `0x03` [Ed25519](https://en.wikipedia.org/wiki/EdDSA)     | `rlp([key, R, s])`                                          |

For non-recoverable signature types (`0x01`, `0x02`, `0x03`), the `key` field carries the signer's `AuthorizedKey` (auth type + raw key data). For secp256k1 (`0x00`), the key is recovered from the signature via `ecrecover` and is not included in the payload.

The `ipa_proof` field contains a serialized [Bulletproofs](https://eprint.iacr.org/2017/1066) inner product argument (§3.5.5) proving the signer's key hash is a component of the committed vector in `key_commitment`:

```
ipa_proof = rlp([
    L_1, R_1,       // round 1 cross-commitments (compressed G₁ points, 32 bytes each)
    L_2, R_2,       // round 2
    L_3, R_3,       // round 3
    L_4, R_4,       // round 4
    L_5, R_5,       // round 5  (k = log₂(N) = 5 rounds)
    a, b,           // final scalars (32 bytes each)
    j               // signer's key index in the committed vector
])
```

## Backwards Compatibility

TODO:

---

## Security Considerations

#### Proof System

**Stale Merkle root.** The `WorldIDRegistry` accepts roots within a validity window. An attacker whose credential has been revoked could race to submit `createAccount` against a still-valid but stale root before expiry. The `expiresAtMin + minExpirationThreshold ≥ block.timestamp` check bounds this window, but its duration is a trust parameter.

**Dual-proof identity binding.** $\pi_c$ and $\pi_a$ are verified independently; the on-chain verifier sees two unlinked nullifiers. A malicious actor controlling two credentials could submit $\pi_c$ from identity $A$ and $\pi_a$ from identity $B$, anchoring $B$'s key set to $A$'s generation counter. Mitigation requires either a shared session binding or an explicit same-leaf circuit constraint.

**OPRF key rotation.** If the OPRF key in `OprfKeyRegistry[480]` is rotated, all nullifier derivations change. Existing $\nu_c$ values become unreachable — the generation counter is orphaned and the identity cannot recycle. The protocol must define migration semantics across OPRF key epochs.

#### Transaction Type `0x6f`

**Sender derivation.** The sender is derived from `account_nullifier` (field 10) and `key_commitment` (field 11): $\mathrm{addr} = \mathrm{bytes20}(\mathrm{keccak256}(\nu_a \;\|\; \mathrm{compress}(C)))$. The commitment is self-certifying — if the attacker modifies $C$, the derived address changes and the transaction is rejected. Implementations MUST reject `0x6f` transactions whose derived sender has no `WORLD_ID_ACCOUNT_DELEGATE` delegation designator set.

**Key membership soundness.** The inner product argument $\pi_{\mathrm{ipa}}$ proves that the signer's key hash is a component of the committed vector. Under the discrete logarithm assumption over BN254, the prover cannot forge a valid IPA proof for a key hash that was not committed. The commitment is binding: once $C$ is embedded in the address, the key set is immutable.

**Commitment point malleability.** The compressed BN254 point in field 11 must be validated as a well-formed $G_1$ point. Implementations MUST reject transactions where `key_commitment` does not decompress to a valid curve point, as malformed points could lead to undefined behavior in the IPA verifier.

**WebAuthn origin binding.** The WebAuthn payload includes `client_data_json` which encodes the relying party origin. The `signing_hash` MUST appear as the `challenge` field within `client_data_json`; otherwise the signature could be replayed from a different WebAuthn context.

**Blinding factor entropy.** The hiding property of the Pedersen commitment depends on the blinding factor $\rho$ being drawn from a uniform distribution over $\mathbb{F}_r$. If $\rho$ is predictable or low-entropy, an observer may be able to brute-force the key hashes from $C$. Wallet implementations MUST use a cryptographically secure random number generator for $\rho$.

#### Front-running `createAccount`

The dual proofs and key set are visible in the mempool before inclusion. Signal binding **(G5)** prevents an attacker from substituting a different key set. However, a griefing actor can relay the exact calldata verbatim; the original submitter's transaction then reverts on the nullifier uniqueness check (step ④). Since the account is initialized with the legitimate key set regardless of who submits, the harm is limited to wasted gas. If stronger guarantees are required, the factory could bind the proof to `msg.sender` or require a nonce signed by one of the committed keys.

#### Privacy

**Address-nullifier linkability.** The address is derived as $\mathrm{addr}(\nu_a, C) = \mathrm{bytes20}(\mathrm{keccak256}(\nu_a \;\|\; \mathrm{compress}(C)))$ where $C$ includes a random blinding factor. The address reveals no information about the key set. The account nullifier $\nu_a$ is a public input to $\pi_a$ at `createAccount` time, linking the address to its nullifier for observers of the creation transaction. The nullifier itself is unlinkable to the underlying identity by the ZK property of the OPRF proof.

**Cross-generation linkability (state level).** The `generations` mapping is indexed by a blinded key $\eta = \mathrm{keccak256}(\nu_c \;\|\; \rho_c)$ (§3.5.6.1). Each generation uses a fresh $\rho_c$, producing pseudorandom mapping keys. Scanning on-chain state reveals no grouping of entries by identity. The generational commitment set $\mathcal{S}$ contains computationally hiding Pedersen commitments — no identity linkage is derivable from $\mathcal{S}$ alone.

**Cross-generation linkability (calldata level).** The creation nullifier $\nu_c$ remains a public input to $\pi_c$ and is visible in `createAccount` calldata. Two `createAccount` transactions from the same identity sharing $\nu_c$ are linkable by an observer monitoring transaction submission. This is the sole remaining linkage vector without circuit modifications. The blinded index and OOM proof ensure this linkage does not extend to on-chain state or to transaction-time behavior.

**Recycling privacy.** At account recycling ($g > 0$), the one-out-of-many proof (§3.5.6.3) demonstrates the caller owned *some* prior commitment in $\mathcal{S}$ without revealing which one. The anonymity set is $|\mathcal{S}|$ — every World ID Account ever created. An observer learns only that a valid prior account existed, not which address, generation, or key set it used.

**Key set privacy.** The blinded Pedersen commitment is computationally hiding — the key hashes cannot be extracted from $C$ without the blinding factor $\rho$. During transaction signing, only the signer's individual key is revealed (via the signature payload for non-recoverable schemes, or via ecrecover for secp256k1). The remaining keys in the set stay hidden behind $C$.

**Transaction-time privacy.** The `0x6f` transaction envelope contains no OOM proof, no blinding factors, and no generation index. The sender's identity is authenticated solely via signature and IPA key membership against the self-certifying address $\mathrm{addr}(\nu_a, C)$. The account's generational history is invisible at transaction time.

**Delegated key deanonymization.** If a user delegates a key (e.g., Secp256k1) that is also used as an EOA elsewhere, the linkage between the World ID Account and the EOA is trivially observable when that key signs a transaction. The Pedersen commitment does not protect against this — it only hides keys that have not yet been used to sign.

#### One-Out-of-Many Proof

**Anonymity set growth.** The commitment set $\mathcal{S}$ grows monotonically. OOM proof verification cost is $O(|\mathcal{S}|)$ in the MSM. At $|\mathcal{S}| > 2^{16}$, native precompile verification may approach block-time limits. Implementations SHOULD consider epoch-based partitioning of $\mathcal{S}$ with Merkle-indexed subsets to cap per-proof MSM size, at the cost of reducing the anonymity set to the partition size.

**Commitment set integrity.** The commitment set $\mathcal{S}$ is append-only and managed exclusively by the stateful precompile. Implementations MUST ensure no external write access to $\mathcal{S}$. A malformed commitment (not on BN254 $G_1$, or a commitment to a value not verified by a Groth16 proof) would compromise OOM proof soundness.

**Blinded index replay.** The blinding factor $\rho_c$ is chosen by the caller. A malicious caller could reuse a previous $\rho_c$, causing the new blinded index $\eta$ to collide with a prior (deleted) entry. Since deleted entries map to zero, this would reset the generation counter, potentially enabling account re-creation at a previously used generation. Implementations MUST check that $\mathtt{generations}[\eta_{\mathrm{new}}] = 0$ before writing, ensuring the new blinded index is fresh.

**Blinding factor entropy.** The hiding property of the nullifier commitment $C_g^{\nu}$ and the unlinkability of the blinded index $\eta$ both depend on $\rho_g^{\nu}$ and $\rho_c$ being drawn from uniform distributions over $\mathbb{F}_r$. Wallet implementations MUST use a cryptographically secure random number generator for all blinding factors.

#### EIP-7702 Interactions

**Delegation designator mutability.** [EIP-7702](https://eips.ethereum.org/EIPS/eip-7702) allows an EOA to set its delegation target. If a `0x6f` transaction includes an `authorization_list` (field 9), an authorized key holder could re-delegate away from `WORLD_ID_ACCOUNT_DELEGATE` to arbitrary code, bypassing authentication logic. Implementations MUST either ignore the `authorization_list` for `0x6f` transactions or restrict it to the `WORLD_ID_ACCOUNT_DELEGATE` target exclusively.

---

