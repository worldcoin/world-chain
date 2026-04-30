---
wip: <number>
title: ERC-20 Payment Protection
description: Per-(account, token) ERC-20 transfer limits enforced at the protocol level, with above-limit transfers requiring an admin authorization against the WIP-1001 admin authority.
author: Kilian Glas (@kilianglas)
status: Draft
type: Standards Track
category: Core
created: 2026-04-30
requires: WIP-1001
---

## Abstract

A *Payment Protection* policy bounds the rate at which a [WIP-1001](./wip-1001.md) World Chain Account can transfer a given ERC-20 token under session-key authority alone. Each `(account, token)` pair stores a `Limit`; transfers whose amount fits the limit pass with the ordinary `0x1D` session-key signature, and transfers whose amount exceeds the limit MUST additionally carry an *admin authorization* — a verification by WIP-1001's admin authority bound to the specific transfer's `(account, token, recipient, amount, nonce)`. Above-limit transfers do NOT consume the limit; they are a separate authority, not a credit against it. The policy is admin-type-agnostic: WorldID-admin accounts gain a liveness check (Session Proof), key-admin accounts gain hot/cold separation (cold-key signature), via the same `verifyAdmin` primitive.

Policy state lives in a new stateful precompile, `PAYMENT_PROTECTION_PRECOMPILE`. Enforcement is performed during `0x1D` validation by parsing the outer-call ERC-20 calldata; non-ERC-20 calls and calls to unregistered tokens pass unaffected. Defaults are permissive — a `setLimit` call (itself admin-authorized) opts an `(account, token)` pair into the policy.

## Motivation

Under WIP-1001, a stolen session key can drain account-controlled value at the rate session keys can sign — bounded only by the keyring-mutation timelock, which gates the *keyring* but not the *value flow*. For accounts holding meaningful balances, this is the primary residual risk after WIP-1001's recovery and timelock guarantees: the admin authority can recover access, but cannot prevent depletion in the meantime.

Payment Protection closes this gap by adding a *value-flow* gate alongside WIP-1001's *keyring-mutation* gate. The session key alone authorizes routine spend; the admin authority is required for spend that exceeds a user-configured limit. The mechanism is admin-type-agnostic so it composes uniformly with all WIP-1001 instantiations: WorldID admin contributes liveness (a fresh Session Proof requires the human authenticator), key admin contributes hot/cold separation (cold key signs above-limit transfers; hot session keys cannot).

Restricting v1 to ERC-20 `transfer` / `transferFrom` against registered tokens is a pragmatic scope choice — the bulk of user-held value on World Chain is ERC-20 (notably WLD), the calldata shape is canonical, and the registration step doubles as a clean opt-in. Generalizations (ETH, ERC-721, swaps, `permit`-style flows) are deferred.

## Specification

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119) and [RFC 8174](https://www.rfc-editor.org/rfc/rfc8174).

### Constants

| Name                              | Value                                              | Description                                                                                |
| --------------------------------- | -------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `PAYMENT_PROTECTION_PRECOMPILE`   | TBD                                                | Stateful precompile managing the per-`(account, token)` Limit Map and per-account nonce   |
| `HIGH_VALUE_TRANSFER_TAG`         | `keccak256("PaymentProtection/highValueTransfer")` | Domain tag for high-value transfer admin authorizations                                    |
| `SET_LIMIT_TAG`                   | `keccak256("PaymentProtection/setLimit")`          | Domain tag for `setLimit` admin authorizations                                             |
| `ERC20_TRANSFER_SELECTOR`         | `0xa9059cbb`                                       | Selector for `transfer(address,uint256)`                                                   |
| `ERC20_TRANSFER_FROM_SELECTOR`    | `0x23b872dd`                                       | Selector for `transferFrom(address,address,uint256)`                                       |

### Limit

A `Limit` MUST express a per-`(account, token)` policy that decides, for a given transfer `amount` at a given `block.timestamp`, whether the transfer is within the limit. The exact shape — per-tx threshold, tumbling window, sliding window (token bucket), or other — is **TBD** and is an open question for community discussion (see [Open Questions](#open-questions)). For the remainder of this specification, `Limit` is treated as an opaque type whose semantics expose three operations:

```
within(limit, amount, now) → bool          // does this amount fit the current state
deduct(limit, amount, now) → Limit         // post-pass state update (no-op for stateless shapes)
clamp(limit, newLimit, now) → Limit        // applied on setLimit; no retroactive allowance
```

A `Limit` value with `max = type(uint256).max` MUST be the canonical permissive default; an `(account, token)` pair with no entry in the Limit Map MUST be treated as if its `Limit` were the permissive default.

### Account State

Per-account state stored in `PAYMENT_PROTECTION_PRECOMPILE`:

```
struct PaymentProtectionAccount {
    mapping(address token => Limit) limits;     // (account, token) → Limit; absent ≡ permissive
    uint256                         highValueNonce;  // monotonic; replay protection
}
```

`highValueNonce` is initialized to `0` at first use and incremented by `1` on every successful `verifyAndConsume` and `setLimit`. It is independent of WIP-1001's `adminNonce`.

Single-counter scope (per-account, not per-`(account, token)`) is intentional: high-value transfers and `setLimit` calls serialize across the account, accepting at-most-one in-flight admin-authorized payment-protection operation per account in exchange for a simpler replay-protection contract.

### Precompile Interface

```solidity
interface IPaymentProtection {
    /// @notice Verify an above-limit transfer's admin authorization and bump the nonce.
    /// MUST NOT debit the limit — admin-authorized transfers bypass the limit, they do not consume it.
    function verifyAndConsume(
        address account,
        address token,
        address recipient,
        uint256 amount,
        uint256 nonce,                  // MUST equal stored highValueNonce
        bytes   calldata adminAuthorization
    ) external;

    /// @notice Update the (account, token) Limit. Admin-authorized.
    function setLimit(
        address account,
        address token,
        Limit   calldata newLimit,
        uint256 nonce,
        bytes   calldata adminAuthorization
    ) external;

    // Reads
    function getLimit(address account, address token) external view returns (Limit memory);
    function getHighValueNonce(address account) external view returns (uint256);
}
```

`adminAuthorization` is the opaque bytes blob defined by WIP-1001's per-instantiation `adminAuthorization` layout. The precompile MUST delegate verification to the WIP-1001 admin primitive (see [Admin Authorization](#admin-authorization)).

### Signal Binding

`PAYMENT_PROTECTION_PRECOMPILE` MUST recompute the `signalHash` for each operation from its call parameters; it MUST NOT accept a caller-supplied `signalHash`. This makes the binding frontrun-safe by construction — an admin authorization observed in the mempool is bound to one specific `(token, recipient, amount, nonce)` quadruple and cannot be re-bound to a different transfer.

```solidity
struct HighValueTransferSignal {
    bytes32 tag;            // == HIGH_VALUE_TRANSFER_TAG
    address account;
    address token;
    address recipient;
    uint256 amount;
    uint256 nonce;
}

struct SetLimitSignal {
    bytes32 tag;            // == SET_LIMIT_TAG
    address account;
    address token;
    Limit   newLimit;
    uint256 nonce;
}

signalHash = uint256(keccak256(abi.encode(signal))) >> 8
```

The `>> 8` truncation reduces the digest to 248 bits to fit within the BN254 scalar field, matching WIP-1001's signal width. Truncation is uniform across all admin instantiations. The domain tags (`HIGH_VALUE_TRANSFER_TAG`, `SET_LIMIT_TAG`) are distinct from WIP-1001's keyring-update tags, ensuring an admin authorization for one operation class cannot be replayed against another.

For `verifyAndConsume`, `(token, recipient, amount)` MUST be sourced from the parsed ERC-20 calldata of the enclosing `0x1D` transaction (see [Validation Flow](#validation-flow)); `account` is the `0x1D` envelope's `account` field. For `setLimit`, all signal fields are call parameters.

### Admin Authorization

Admin verification is delegated to a public read-style entry point on the WIP-1001 precompile (see [Required WIP-1001 Extension](#required-wip-1001-extension)):

```
WORLD_CHAIN_ACCOUNT_PRECOMPILE.verifyAdmin(account, signalHash, adminAuthorization)
```

The entry point dispatches on the account's stored `adminType` and `verificationPublicId`, applying the per-instantiation verification primitive defined by WIP-1001:

- **`ADMIN_TYPE_WORLD_ID`** — Session Proof against the stored `sessionId`. A Session Proof is sufficient; a Uniqueness Proof gives no additional security at higher proving cost.
- **`ADMIN_TYPE_SECP256K1`** — secp256k1 ECDSA over `signalHash`, low-`s` enforced.
- **`ADMIN_TYPE_P256`** — P256 ECDSA over `signalHash`, low-`s` enforced; verification via [RIP-7212](https://github.com/ethereum/RIPs/blob/master/RIPS/rip-7212.md).

`verifyAdmin` MUST revert on failure. It MUST NOT touch WIP-1001's `adminNonce` — replay protection for payment-protection operations is owned by `highValueNonce`, not by the admin authority's keyring-update counter.

### `0x1D` Envelope Extension

The `0x1D` envelope ([WIP-1001](./wip-1001.md)) is extended with one OPTIONAL field:

```
high_value_authorization = rlp([nonce, adminAuthorization])
```

`nonce` is a `uint256` and MUST equal the account's stored `highValueNonce` at validation time. `adminAuthorization` is the bytes blob defined by WIP-1001's per-instantiation layout for the account's `adminType`. The field is OPTIONAL: a transfer that fits the limit MUST NOT include it, and a transfer that exceeds the limit MUST include it.

When omitted, the field MUST be encoded as the empty RLP item; this preserves the canonical `0x1D` `signing_hash` computation for transactions that do not require payment-protection enforcement.

### Validation Flow

During `0x1D` validation, after the WIP-1001 session-key signature check has succeeded, the protocol MUST execute the following steps for the outer call to `to` with calldata `data`:

1. **Token registration check.** Look up `limits[to]`. If the entry is absent (or equal to the permissive default), skip the remaining steps — the transfer is unconstrained by Payment Protection.
2. **Calldata parse.** If `data` begins with `ERC20_TRANSFER_SELECTOR` and decodes as `transfer(address recipient, uint256 amount)`, set `(recipient, amount) = (recipient, amount)`. Else if `data` begins with `ERC20_TRANSFER_FROM_SELECTOR` and decodes as `transferFrom(address from, address recipient, uint256 amount)` AND `from == account`, set `(recipient, amount)`. Else set `amount = 0` (the call is treated as a non-transfer for limit purposes and passes trivially).
3. **Limit check.** Refresh the stored `Limit` to `now = block.timestamp`. If `within(limits[to], amount, now)` holds, apply `deduct(...)` and pass.
4. **Admin-authorized path.** Otherwise, `high_value_authorization` MUST be present. The protocol MUST invoke `PAYMENT_PROTECTION_PRECOMPILE.verifyAndConsume(account, to, recipient, amount, nonce, adminAuthorization)`, which MUST:
   1. Require `nonce == highValueNonce[account]`.
   2. Recompute `signalHash` per `HighValueTransferSignal { HIGH_VALUE_TRANSFER_TAG, account, to, recipient, amount, nonce }`.
   3. Invoke `WORLD_CHAIN_ACCOUNT_PRECOMPILE.verifyAdmin(account, signalHash, adminAuthorization)`.
   4. Increment `highValueNonce[account]`.
   5. Return without modifying the `Limit` for `(account, to)`.

Any failure in steps 3 or 4 MUST cause the entire `0x1D` validation to fail; the transaction never enters execution. `setLimit` is itself a `0x1D` transaction whose `to` is `PAYMENT_PROTECTION_PRECOMPILE`; admin authorization for it follows the same dispatch and consumes the same `highValueNonce`.

Enforcement applies only to the outer call of the `0x1D` transaction. Inner calls (e.g., a session-key-driven contract call that internally invokes `token.transfer`) are NOT inspected in v1; see [Open Questions](#open-questions).

### Required WIP-1001 Extension

This WIP requires a public read-style entry point on the WIP-1001 precompile that is not present in the WIP-1001 v1 surface:

```solidity
function verifyAdmin(
    address account,
    uint256 signalHash,
    bytes calldata adminAuthorization
) external view;
```

The entry point dispatches on the account's stored `adminType` and `verificationPublicId`, applies the per-instantiation verification primitive, and reverts on failure. It MUST NOT touch `adminNonce` — replay protection is the caller's concern.

This addition MAY land as an extension to WIP-1001 itself or as a sibling WIP. Either way, the abstract admin-verification machinery is already specified in WIP-1001; this extension only exposes it.

### Defaults and Opt-In

New accounts have no `(account, token)` entries; their `Limit` is the permissive default for every token, and Payment Protection is a no-op until the user calls `setLimit`. The first `setLimit` for any `(account, token)` pair is itself admin-authorized — a stolen session key cannot silently disable protection, because there is nothing to disable until the admin authority enables it.

A consequence: payment protection guards *post-funding* session compromise. An attacker who compromises a session key during the brief window between account creation and the first `setLimit` faces no value-flow gate. A chain-wide governance floor would close this gap and is deferred.

## Rationale

**Layering on the WIP-1001 admin authority.** Payment protection is fundamentally a *value-flow* policy that wants to spend from the same authority WIP-1001 already manages for keyring mutations. Re-using `verifyAdmin` rather than introducing a parallel admin verification path means: (a) one identity binding per account, (b) admin rotation extensions land for free, (c) the per-`adminType` security ceiling applies uniformly, and (d) wallets need only one admin-authorization UX flow. The cost is the small WIP-1001 surface extension (`verifyAdmin` made public).

**Independent `highValueNonce`.** Sharing WIP-1001's `adminNonce` would force serialization between high-value transfers and keyring updates with no security benefit and a UX penalty (a pending keyring update would invalidate every above-limit transfer authorization). Per-operation-class nonces are the correct unit of replay protection.

**Single per-account counter, not per-token.** Per-token nonces would let concurrent above-limit transfers proceed across distinct tokens. Per-account nonces accept at-most-one in-flight admin-authorized op in exchange for a single-slot replay-protection contract and simpler reasoning. The trade-off is judged acceptable for v1; per-token nonces remain available as a future revision if observed UX demands them.

**Precompile-recomputed signal.** Binding `(token, recipient, amount)` from the parsed `0x1D` calldata, rather than from caller-supplied parameters, makes admin authorizations frontrun-safe at the protocol level: a witnessed authorization in the mempool is bound to one specific transfer and cannot be re-bound. This is the same defense WIP-1001 applies to keyring-update authorizations via uniform `signalHash` recomputation.

**ERC-20-only, registration-gated, outer-call-only.** Three orthogonal scope cuts that each reduce v1 implementation complexity and risk. ERC-20 is the canonical user-held value class on World Chain; its calldata is a stable, well-known shape. Per-token registration via `setLimit` makes opt-in explicit and side-steps the brittleness of protocol-level token discovery. Outer-call enforcement bounds the implementation to one well-defined hook in `0x1D` validation; deeper-frame inspection (e.g., DEX swaps that internally `transferFrom`) is acknowledged as out of scope, with the trade-off documented in [Security Considerations](#security-considerations).

**Bypass, not consume.** An admin-authorized transfer does not debit the `Limit`. The two paths represent different authorities — session-key-only spend bounded by `Limit`, admin-authorized spend bounded by the admin authority itself — and entangling them would either let an admin-authorized transfer reset the windowed allowance (gameable) or reduce the allowance after an authorized spend (surprising for the user). Keeping them separate matches the conceptual model that the limit gates session-key spend, not all spend.

**Precompile state, not contract state.** Like WIP-1001, payment protection lives in a precompile because its state is read during `0x1D` consensus validation. A contract-side implementation would require the protocol to call into EVM execution from the validation path, doubling the consensus surface and forcing gas-pricing decisions about a path that should be cheap and bounded.

## Backwards Compatibility

This WIP is purely additive at the consensus layer:

- **Accounts existing under WIP-1001.** Unaffected. Payment Protection is opt-in via `setLimit`; an account that never calls `setLimit` sees no behavioral change.
- **`0x1D` envelope.** The `high_value_authorization` field is OPTIONAL with a canonical empty encoding. Pre-fork `0x1D` transactions and post-fork `0x1D` transactions that omit the field hash and validate identically up to the new field's canonical encoding. The protocol MUST gate enforcement on a hardfork activation block; pre-fork blocks MUST NOT apply Payment Protection logic.
- **Non-`0x1D` transactions.** Unaffected. ERC-20 transfers signed by an EOA or routed through ERC-4337 do not pass through the `0x1D` validation hook and are not subject to this WIP.
- **Existing ERC-20 contracts.** Unaffected. The WIP imposes no contract-side requirement; standard ERC-20 implementations are used as-is, with selector-based detection at the `0x1D` validation layer.

## Security Considerations

### Threat Model

Payment Protection guards against *session-key-only compromise*. An attacker who has obtained one or more session keys but not the admin authority cannot move more than the configured `Limit` from any registered token. Admin-authority compromise sits above this threat model and is the WIP-1001 ceiling — payment protection cannot defend against an attacker who can authorize keyring mutations.

### Per-Admin-Type Security Ceiling

Different WIP-1001 admin instantiations contribute different protections:

- **WorldID admin:** liveness — each above-limit transfer requires a fresh Session Proof, which requires interaction with a WorldID authenticator. Session-key compromise alone is insufficient even with full mempool observation.
- **Key admin (Secp256k1, P256):** hot/cold separation — session keys are hot, the admin key is cold. Above-limit transfers require the cold key. Session-key compromise alone is insufficient unless the cold key is co-located.

The shared invariant is that session-key compromise alone cannot exceed `Limit`. The shared *non-*invariant is that admin-authority compromise allows arbitrary spend; payment protection does not strengthen WIP-1001's admin-tier ceiling.

### Key-Admin Bricking

Under WIP-1001, key-admin accounts already face terminal admin-key loss. Payment Protection compounds this: if a key-admin account has set a non-default `Limit` and the admin key is lost, above-limit transfers can no longer be authorized. The account remains operable for below-limit transfers but is permanently capped. Wallets implementing key-admin Payment Protection MUST surface this risk before opt-in. An emergency-override mechanism (e.g., a long-timelocked `disableProtection` path) is deferred — see [Open Questions](#open-questions).

### Frontrun-Safety

Because `signalHash` is recomputed by the precompile from the parsed calldata of the enclosing `0x1D` transaction (and `nonce` is bound into the signal), a witnessed `high_value_authorization` cannot be lifted out of one `0x1D` transaction and reattached to a different one — the recomputed signal would not match. Self-replay by the same submitter is a no-op (the nonce has already been bumped).

### Cross-Operation Replay

Distinct domain tags (`HIGH_VALUE_TRANSFER_TAG`, `SET_LIMIT_TAG`) namespace payment-protection signals away from WIP-1001's keyring-update signals (`WIP-1001/create`, `WIP-1001/update`). An admin authorization produced for a high-value transfer cannot be replayed as a `setLimit`, a `Create`, or an `update`, and vice versa.

### Tumbling-Window Boundary Attack

If the `Limit` shape ultimately chosen exposes a fixed-period rolling cap with a hard rollover, an attacker who controls a session key can drain `2 · max` in a single short window straddling the rollover boundary (full spend at `periodEnd − ε`, full spend at `periodEnd + ε`). A token-bucket sliding-window shape closes this for the same constant-slot state cost. This is a generic finding for any cumulative-cap design and informs the open question on `Limit` shape — see [Open Questions](#open-questions).

### Outer-Call-Only Enforcement

V1 enforces only on the outer call of the `0x1D` transaction. A session key signing a call to a router contract that internally executes `token.transferFrom(account, …, amount)` bypasses Payment Protection — the outer `to` is the router, not the registered token, and step 1 of the validation flow short-circuits. This matches the protocol-versus-application division of responsibility (the protocol enforces against transactions, not against arbitrary call graphs), but means Payment Protection alone does not prevent a stolen session key from draining tokens via approved DEX or aggregator paths. Wallets SHOULD audit pre-existing `approve` allowances on registered tokens and SHOULD encourage time-bounded approvals.

### `permit`-Based Bypass

[ERC-2612](https://eips.ethereum.org/EIPS/eip-2612) `permit` lets a holder produce an off-chain signature authorizing an approval, which any third party can submit and consume. A session key signing a `permit` is a session-key-signed message, not a `0x1D` transaction, and therefore is not gated by Payment Protection. Above-limit drains via `permit` + `transferFrom` are not prevented in v1.

### Onboarding Window

Between account creation and the first `setLimit`, no `(account, token)` policy is set, so Payment Protection is a no-op. An attacker who compromises a session key during this window — e.g., via a poisoned wallet — is unconstrained until the user opts in. A chain-wide minimum policy or an at-creation policy parameter would close this window; both are deferred.

### Calldata-Parse Brittleness

Step 2 of the validation flow relies on selector-based ERC-20 calldata parsing. Tokens that use non-standard selectors (custom names, upgradeable proxies that reroute) are not protected by this scheme. Such tokens lose ecosystem interoperability for unrelated reasons (wallet integrations, indexers, block explorers all rely on the standard selectors) and so the practical attack surface is narrow, but the WIP makes no guarantee for non-standard tokens.

## Open Questions

The following are open for community discussion and are expected to resolve before this WIP advances out of `Draft`.

- **`Limit` shape.** Per-tx threshold, tumbling window, sliding window (token bucket), or other. Trade-offs span state-slot count, SSTORE frequency, boundary-attack resistance, and UX. The sliding-window shape closes the boundary attack at one extra SSTORE per check; the per-tx threshold is the simplest but leaves drain unbounded over time. To be locked before `Review`.
- **Period default** (windowed shapes only) — per-token, per-account, or chain-wide.
- **Per-token vs per-USD limits.** Token-native limits expose users to volatility; USD-denominated limits require an oracle and a staleness policy. Deferred.
- **`verifyAdmin` ownership.** Land the WIP-1001 surface extension in WIP-1001 itself, or as a sibling WIP. Either is technically equivalent.
- **Emergency override for key-admin bricking.** A long-timelocked `disableProtection` path that, after a multi-week delay, reverts an `(account, token)` pair to permissive without admin authorization. Reduces the bricking risk but introduces a new attack window. Deferred.
- **Inner-call enforcement.** Whether v2 should extend enforcement into inner call frames (custom EVM `Inspector` or analogous) to cover DEX-routed and `permit`-mediated transfers. Cost: every call-frame entry pays a registration check.
- **Per-token vs per-account `highValueNonce`.** Concurrency vs simplicity. Per-token would allow concurrent above-limit transfers on different tokens; per-account is the v1 cut.
- **Onboarding-window mitigation.** Chain-wide minimum policy, at-creation policy parameter, or status quo (deferred).

## Copyright

Copyright and related rights waived via [CC0](https://creativecommons.org/publicdomain/zero/1.0/).
