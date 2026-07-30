# WLD Paymaster — Backend-less, fully on-chain ERC-20 Paymaster (POC)

> **Status: POC / MVP. NOT audited.** Paymaster contracts custody funds and are
> high-value attack targets. A full security review is required before any
> production or mainnet deployment. See [Risks](#7-risks--edge-cases).

## 0. Goal

Let World App users pay ERC-4337 gas in **WLD** instead of ETH, with **no
off-chain backend**. Concretely, per leadership's design:

1. The paymaster is funded **once** with ETH (deposited to the EntryPoint). No
   server keeps it topped up.
2. It charges users a **+20% premium** over the current WLD/ETH price to absorb
   price drift, swap fees/slippage, and provide a buffer.
3. It does **not** swap WLD→ETH per UserOp. It accumulates WLD and performs a
   **batched** swap every `X` blocks, then re-deposits the resulting ETH into
   the EntryPoint — self-sustaining after the initial funding.
4. It reads WLD/ETH from an **on-chain oracle** (Uniswap V3 TWAP), abstracted
   behind an interface so it can later be swapped for Chainlink.

This document describes the "backend-less / fully on-chain" variant. It is an
alternative to the ERC-7677 off-chain paymaster-service approach (where a server
signs each UserOp and keeps the deposit topped up).

## 1. Architecture

```
                 ┌──────────────────────────────────────────────────────────┐
                 │                        World Chain                         │
                 │                                                            │
  UserOp (WLD    │   ┌───────────┐  validate/postOp  ┌────────────────────┐  │
  gas) ───────► Bundler ───────► │ EntryPoint │ ────────────────► │   WLDPaymaster     │  │
  (Rundler proxy │   │  v0.7     │                   │                    │  │
   whitelists    │   └───────────┘ ◄── ETH deposit ──│  - validate: pull  │  │
   the paymaster)│         ▲                          │    max WLD (oracle │  │
                 │         │                          │    +20% premium)   │  │
                 │         │ depositTo (replenish)    │  - postOp: refund   │  │
                 │         └──────────────────────────│    unused WLD       │  │
                 │                                     │  - accumulatedWld   │  │
                 │   ┌─────────────────┐   reads       └─────────┬──────────┘  │
                 │   │ IWldEthOracle   │ ◄──────────────────────┘             │
                 │   │ (UniV3 TWAP)    │           triggerBatchSwap(minOut)    │
                 │   └────────┬────────┘        (permissionless, every X blk)  │
                 │            │ observe()                     │                │
                 │   ┌────────▼────────┐   WLD→WETH   ┌───────▼────────┐       │
                 │   │ WLD/WETH V3 pool │ ◄───────────│  UniV3 Router  │       │
                 │   └─────────────────┘   swap       └────────────────┘       │
                 └──────────────────────────────────────────────────────────┘
```

### 1.1 `validatePaymasterUserOp` flow

1. EntryPoint calls `validatePaymasterUserOp(userOp, hash, maxCost)`.
2. Safety: require `getDeposit() >= maxCost + minEntryPointDeposit` so a burst of
   ops cannot drain the deposit below the configured floor mid-batch.
3. Price the op: `base = oracle.wldForEth(maxCost)`, then
   `maxWldCharge = base * (10000 + premiumBps) / 10000` (default +20%).
4. Require the user has `balanceOf >= maxWldCharge` and
   `allowance(user, paymaster) >= maxWldCharge`.
5. **Pull the maximum charge up-front** with `transferFrom` (see
   [§3 Collection](#3-collecting-wld-from-the-user)).
6. Return `context = (sender, maxWldCharge, maxCost)` and `validationData = 0`.

### 1.2 `postOp` flow

1. EntryPoint calls `postOp(mode, context, actualGasCost, actualUserOpFeePerGas)`.
2. Estimate the true cost including postOp's own gas:
   `costWithPostOp = actualGasCost + postOpGasOverhead * actualUserOpFeePerGas`,
   capped at `maxCost`.
3. Charge pro-rata (premium is already baked into `maxWldCharge`):
   `actualWldCharge = maxWldCharge * costWithPostOp / maxCost`.
4. `accumulatedWld += actualWldCharge`; refund `maxWldCharge - actualWldCharge`
   to the user.

`postOp` is always requested (non-empty context). Even on `opReverted`, the
paymaster still pays gas to the EntryPoint, so it still charges the user — the
up-front pull guarantees the WLD is already in hand.

## 2. Pricing: on-chain oracle + premium

The paymaster depends only on `IWldEthOracle`:

```solidity
interface IWldEthOracle {
    function wldForEth(uint256 ethWei) external view returns (uint256 wldAmount);
    function ethForWld(uint256 wldAmount) external view returns (uint256 ethWei);
}
```

**MVP implementation — `UniswapV3TwapOracle`:** reads a time-weighted average
tick over `twapWindow` seconds from the WLD/WETH V3 pool via `pool.observe()`,
and converts amounts with the standard `OracleLibrary.getQuoteAtTick`
(vendored to solc ^0.8 in `src/vendor/`). Using a TWAP (not spot) is the primary
manipulation mitigation: a spot spike must be *sustained across the whole
window* to move the reported price.

**Premium.** `premiumBps` (default `2000` = +20%) is applied on top of the oracle
price in `validate`. Because the up-front charge already includes the premium
and `postOp` scales it linearly, the effective charge is always
`1.2 × (WLD-equivalent of actual gas)`.

**Chainlink note.** The interface is deliberately implementation-agnostic so a
Chainlink adapter can replace the TWAP oracle via `setOracle(...)` with zero
paymaster changes. **Assumption (not verified):** at time of writing we assume
there is *no* canonical Chainlink WLD/ETH price feed live on World Chain, so the
Uniswap V3 TWAP is the default. If/when a Chainlink WLD/ETH (or WLD/USD + ETH/USD)
feed exists, wrap it behind `IWldEthOracle` and switch. Chainlink would remove
in-protocol TWAP manipulation surface but adds a trust/liveness dependency.

## 3. Collecting WLD from the user

The MVP pulls the **maximum** WLD charge in `validate` via `transferFrom`, then
refunds the unused portion in `postOp`. This requires the user (smart account) to
have granted an ERC-20 allowance to the paymaster beforehand (a one-time
`approve`, or `permit`/`Permit2` bundled into the UserOp `callData`).

**Why pull up-front rather than in `postOp`?** If we only *checked* balance in
`validate` and pulled in `postOp`, a malicious account could move its WLD during
its own execution phase (between validate and postOp), making the `postOp`
`transferFrom` fail — but the paymaster has *already* paid the EntryPoint for
gas, so it would eat the loss. Pulling up-front removes that race.

> **Note on ERC-4337 storage rules.** Writing state and calling `transferFrom` in
> `validate` normally violates bundler simulation/reputation rules for
> *un-staked/un-whitelisted* paymasters. This design is safe **because the
> paymaster is whitelisted** on the Rundler proxy sidecar (see
> [§8 Whitelisting](#8-whitelisting)). A non-whitelisted deployment MUST instead
> only read balance/allowance in `validate` and pull in `postOp`, accepting the
> race above (or require pre-deposit/escrow).

## 4. Batching: "every X blocks"

`triggerBatchSwap(minEthOut)` is **permissionless** and reverts with
`BatchTooEarly` until `block.number >= lastBatchBlock + blocksPerBatch`. On a
successful call it:

1. Snapshots `amountIn = accumulatedWld`, then (checks-effects-interactions)
   resets `accumulatedWld = 0` and `lastBatchBlock = block.number` **before** any
   external calls.
2. Computes an oracle-bounded floor:
   `floor = max(minEthOut, oracle.ethForWld(amountIn) * (10000 - maxSwapSlippageBps)/10000)`.
3. Swaps WLD→WETH via the Uniswap V3 router (`amountOutMinimum = floor`),
   unwraps WETH→ETH, optionally pays a keeper reward, and re-deposits the rest to
   the EntryPoint.

### 4.1 Trigger strategy — recommendation

"Fully backend-less" means we cannot *rely* on an off-chain keeper. Options:

| Approach | Pros | Cons |
|---|---|---|
| **Permissionless crank (chosen)** | No backend; anyone can call; owner/searcher/next-op can trigger | Needs *someone* to call; without incentive WLD may sit un-swapped |
| Auto-trigger on next UserOp after threshold | Truly zero external actor | Puts an expensive swap in a user's op → unpredictable/large gas, DoS/MEV surface, complicates `validate` gas rules |
| Off-chain keeper / Chainlink Automation | Reliable cadence | Reintroduces a backend dependency (the thing we're avoiding) |

**Recommendation: permissionless crank (implemented), with an optional built-in
`keeperRewardBps`** paid from swap proceeds to whoever calls it. This keeps the
system backend-less by default while making it *rational* for a searcher to crank
the batch once the accumulated WLD is worth more than gas + reward. We
deliberately **did not** auto-trigger inside a UserOp: forcing a full Uniswap
swap into an arbitrary user's `postOp` creates unbounded, unfair gas costs and a
griefing/MEV surface. Chainlink Automation remains available as a drop-in later
(it just calls the same public function).

## 5. EntryPoint deposit management

- **Initial funding:** owner calls `deposit()` once with ETH → `depositTo(paymaster)`.
- **Replenishment:** each `triggerBatchSwap` re-deposits ETH proceeds, closing
  the loop (WLD in → ETH out → deposit).
- **Safety floor:** `validate` requires `getDeposit() >= maxCost + minEntryPointDeposit`.
  If the deposit is running low the paymaster rejects new ops (fail-safe) rather
  than risking a mid-batch shortfall. `batchReady()` and `getDeposit()` are views
  a monitor/keeper can watch.
- **Withdrawals:** `withdrawTo`/`sweepExcessWld` (owner-only) for recovery.
  `sweepExcessWld` explicitly cannot touch `accumulatedWld`.

## 6. Configurability (owner)

| Param | Default | Meaning |
|---|---|---|
| `premiumBps` | 2000 (+20%) | Premium over oracle price (capped at +100%) |
| `blocksPerBatch` (X) | 300 | Min blocks between batch swaps |
| `maxSwapSlippageBps` | 300 (3%) | Oracle-bounded min-out for the batch swap |
| `swapPoolFee` | ctor arg | Uniswap V3 fee tier for the swap |
| `minEntryPointDeposit` | 0.05 ETH | Deposit floor preserved after each op |
| `postOpGasOverhead` | 40000 | Gas assumed for postOp, folded into the charge |
| `keeperRewardBps` | 0 | Optional reward to the batch-swap caller |
| `oracle` | ctor arg | Swappable `IWldEthOracle` (TWAP → Chainlink later) |

## 7. Risks & edge cases

- **TWAP manipulation.** Short windows are cheaper to manipulate. Mitigations:
  use a sufficiently long `twapWindow`, prefer a deep-liquidity fee tier, keep
  the +20% premium as a buffer, and cap per-op exposure via `minEntryPointDeposit`.
  A determined attacker who sustains an off-market price for the whole window
  could under-pay for gas; the premium and batch slippage bound limit the bleed.
- **Batch-swap slippage / thin liquidity.** `amountOutMinimum` is set from the
  oracle price minus `maxSwapSlippageBps`; if the pool can't fill at that price
  the swap reverts (WLD stays accumulated, retried next window). **Liquidity
  assumption:** a WLD/WETH V3 pool with enough depth exists on World Chain's
  Uniswap; batch size should stay within what that pool can absorb inside the
  slippage bound (tune `blocksPerBatch` so batches don't grow too large).
- **Oracle == swap venue.** Both the price and the swap use Uniswap; an attacker
  who moves the pool moves both. A Chainlink price oracle (independent of the
  swap venue) would decorrelate these and is the recommended production upgrade.
- **Deposit runs low.** The floor check rejects new ops before the deposit is
  exhausted; ops are simply un-sponsored until a batch (or the owner) replenishes.
- **Front-running / MEV on the batch trigger.** The swap direction and size are
  public once WLD accumulates. A searcher could sandwich the batch swap. The
  oracle-based `amountOutMinimum` bounds the damage; further hardening could
  route through an aggregator or split batches. `keeperRewardBps` intentionally
  turns "who triggers" into a public, incentive-aligned race rather than a
  privileged action.
- **Premium vs. real cost.** If WLD depreciates faster than +20% between charge
  and batch settlement, the paymaster can still run a deficit. The premium and
  batch cadence are the only buffers; monitor and tune.
- **Griefing via reverting user ops.** Even if a user op reverts, the up-front
  WLD pull means the paymaster is paid, so this is not economically exploitable.

## 8. Whitelisting

For this design to work at all, the paymaster address must be **whitelisted on
World Chain's Rundler proxy sidecar**. Whitelisting:

- Exempts the paymaster from ERC-4337 storage/reputation rules, so state writes
  and `transferFrom` in `validate` are accepted (this is what makes the
  up-front-charge model viable and removes the need for a large EntryPoint stake
  purely for reputation).
- Must **also** be added (at least temporarily) to any third-party bundlers used
  (Alchemy / Pimlico), since they refuse to bundle UserOps for paymasters they
  have not whitelisted.

The paymaster still needs a real ETH deposit on the EntryPoint to actually pay
for the gas it sponsors — whitelisting only removes the *reputation-stake*
requirement, not the funding requirement.

## 9. Out of scope for the MVP

Audited-grade hardening, permit/Permit2 collection path, multi-hop swap routing,
per-user rate limiting, pausability/circuit breakers, upgradeability, and a
production oracle (Chainlink) integration. These are the recommended follow-ups.
