# wld-paymaster-poc

A **proof-of-concept**, fully on-chain, **backend-less** ERC-20 Paymaster that
lets World App users pay ERC-4337 (account abstraction) gas in **WLD** instead of
ETH on **World Chain**.

> ⚠️ **POC / MVP — NOT AUDITED.** Paymaster contracts custody funds and are
> high-value attack targets. This code is for design exploration only and
> **must undergo a full security review before any production/mainnet use.**

## What it does

- Prices each UserOp's gas in WLD via **Chainlink** feeds (WLD/USD × ETH/USD
  cross), abstracted behind `IWldEthOracle`. The Uniswap V3 TWAP oracle is kept
  as a swappable fallback implementation.
- Charges the user a **+20% premium** over the oracle price to absorb price
  drift, swap fees/slippage, and provide a buffer.
- **No per-op swap** and **no backend server.** It accumulates WLD and, via a
  **permissionless** `triggerBatchSwap()` callable every `X` blocks, swaps up to
  `maxWldPerBatch` WLD→ETH on Uniswap **SwapRouter02** (with oracle-bounded
  slippage protection) and **re-deposits the ETH into the EntryPoint** —
  self-sustaining after a one-time funding.

Full write-up: [**DESIGN.md**](./DESIGN.md).

## Layout

```
src/
  WLDPaymaster.sol                 # main paymaster (BasePaymaster / IPaymaster, EntryPoint v0.7)
  interfaces/
    IWldEthOracle.sol              # price-oracle abstraction (Chainlink default, TWAP fallback)
    IAggregatorV3.sol              # Chainlink AggregatorV3 read surface (from the live feed's ABI)
    ISwapRouter.sol                # minimal Uniswap V3 SwapRouter + WETH9
    IUniswapV3PoolMinimal.sol      # pool.observe() subset for TWAP
  oracle/
    ChainlinkWldEthOracle.sol      # DEFAULT: WLD/USD x ETH/USD Chainlink cross
    UniswapV3TwapOracle.sol        # fallback: IWldEthOracle backed by a WLD/WETH V3 pool TWAP
  vendor/                          # TickMath / FullMath / OracleLibrary ported to solc ^0.8
test/
  WLDPaymaster.t.sol               # validate/postOp, premium math, batching, edge cases
  ChainlinkWldEthOracle.t.sol      # cross math, decimal normalisation, stale/invalid feeds
  ChainlinkWldEthOracle.fork.t.sol # optional: live World Chain feeds (needs WORLDCHAIN_RPC_URL)
  EntryPointIntegration.t.sol      # real EntryPoint.handleOps: prefund ordering, floor, refunds
  E2E.fork.t.sol                   # optional: full loop vs live EntryPoint/WLD/router/pool
  Deploy.fork.t.sol                # optional: runs the deploy script, asserts it can sponsor
  Unwind.fork.t.sol                # optional: asserts teardown recovers every wei and token
  UniswapV3TwapOracle.t.sol        # TWAP conversion via a mock V3 pool
  mocks/Mocks.sol                  # ERC20 / WETH / oracle / aggregator / router / pool mocks
script/
  Deploy.s.sol                     # deploy + configure + fund + assert ready-to-sponsor
  CheckReady.s.sol                 # read-only: can a deployed paymaster sponsor right now?
  Unwind.s.sol                     # teardown: recover deposit + stake + WLD for redeploy
```

## Build & test

```bash
# install dependencies (pinned versions used by this POC)
forge install foundry-rs/forge-std
forge install OpenZeppelin/openzeppelin-contracts@v5.0.2
forge install eth-infinitism/account-abstraction@v0.7.0

forge build
forge test -vvv

# optional but recommended: run the fork tests against live World Chain contracts
WORLDCHAIN_RPC_URL=https://worldchain-mainnet.g.alchemy.com/public \
  forge test --match-path 'test/*.fork.t.sol' -vv
```

Expected: **47 passing unit tests**, plus **19 fork tests** that are skipped unless
`WORLDCHAIN_RPC_URL` is set. The fork suite is what catches integration breakage
the mocks cannot — `E2E.fork.t.sol` runs charge → reconcile → swap → re-deposit
against the real EntryPoint, WLD, SwapRouter02 and WLD/WETH pool.

## Live World Chain addresses (chain id 480)

Baked in as defaults in `script/Deploy.s.sol` and asserted by `E2E.fork.t.sol`.

| What | Address |
|---|---|
| EntryPoint v0.7 | `0x0000000071727De22E5E9d8BAf0edAc6f37da032` |
| WLD | `0x2cFc85d8E48F8EAB294be644d9E25C3030863003` |
| WETH9 | `0x4200000000000000000000000000000000000006` |
| **SwapRouter02** | `0x091AD9e2e6e5eD44c1c66dB50e49A601F9f36cF6` |
| WLD/WETH pool (0.3%) | `0x494D68e3cAb640fa50F4c1B3E2499698D1a173A0` |

⚠️ World Chain has **only SwapRouter02**, whose `exactInputSingle` struct has **no
`deadline` field** (selector `0x04e45aaf`). The legacy v3-periphery `SwapRouter`
is not deployed. Using the deadline variant makes every batch swap revert — see
`src/interfaces/ISwapRouter.sol`.

⚠️ The 0.3% tier is the only WLD/WETH pool with real liquidity (~313k WLD / ~9.6
WETH), and it is **shallow on the WETH side**. Hence `maxWldPerBatch` (default
500 WLD, ~0.7% price impact measured on-chain): without a cap, a large backlog
would exceed `maxSwapSlippageBps` on every attempt and stall settlement forever.

## Price feeds (World Chain mainnet, chain id 480)

Both are `ChainlinkPriceFeed` contracts (Chainlink Data Streams verifier wrappers
exposing `AggregatorV3Interface`) and report **18 decimals**, not mainnet's 8 —
`ChainlinkWldEthOracle` reads `decimals()` on each feed rather than assuming.

| Feed | Address | Decimals |
|---|---|---|
| WLD/USD | `0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0` | 18 |
| ETH/USD | `0xe1d72a719171DceAB9499757EB9d5AEb9e8D64A6` | 18 |

## Key parameters (owner-configurable)

| Param | Default | Meaning |
|---|---|---|
| `premiumBps` | 2000 (+20%) | Premium over oracle price |
| `blocksPerBatch` (X) | 300 | Min blocks between batch swaps |
| `maxSwapSlippageBps` | 300 (3%) | Oracle-bounded min-out on the batch swap |
| `minEntryPointDeposit` | 0.05 ETH | Deposit floor that must remain after each op (compared against the post-prefund balance) |
| `postOpGasOverhead` | 40000 | Gas assumed for postOp, folded into the charge |
| `keeperRewardBps` | 0 | Optional reward paid to the batch-swap caller |
| `maxWldPerBatch` | 500 WLD | Max WLD sold per batch; bounds price impact (0 = unlimited) |
| `oracle` | ctor | Swappable `IWldEthOracle` (Chainlink default) |
| `maxStaleness` | 1 hour (oracle ctor) | Max feed answer age before ops are rejected |

## Deploy & configure

`script/Deploy.s.sol` does the whole thing: deploys the oracle and paymaster,
applies every config knob, deposits, stakes, optionally hands ownership to a
multisig, and then **asserts the result can actually sponsor** before returning.
It aborts on any unmet precondition rather than leaving a half-configured
paymaster on-chain.

```bash
export WORLDCHAIN_RPC_URL=https://worldchain-mainnet.g.alchemy.com/public

# dry run (no --broadcast): pre-flight + full report, nothing sent
forge script script/Deploy.s.sol:Deploy --rpc-url "$WORLDCHAIN_RPC_URL" -vvv

# for real
OWNER=<multisig> forge script script/Deploy.s.sol:Deploy \
  --rpc-url "$WORLDCHAIN_RPC_URL" --private-key "$PK" --broadcast
```

Every address and parameter has a working World Chain default; override via env
(`DEPOSIT`, `STAKE`, `UNSTAKE_DELAY`, `PREMIUM_BPS`, `MAX_WLD_PER_BATCH`,
`MIN_ENTRYPOINT_DEPOSIT`, `OWNER`, …). See the script header for the full list.

Verify a live deployment at any time — read-only, sends nothing:

```bash
PAYMASTER=0x... MAX_COST=10000000000000 USER=0x... \
  forge script script/CheckReady.s.sol:CheckReady --rpc-url "$WORLDCHAIN_RPC_URL"
```

```
[ok]   oracle live; WLD charge for maxCost: 74558353089106522
       deposit: 20000000000000000  floor: 2000000000000000
[ok]   deposit covers maxCost + floor; ops sponsorable: 1800
[ok]   staked: 50000000000000000  unstake delay: 86400
[ok]   max WLD per batch: 500000000000000000000
=> READY to sponsor.
```

## Teardown / redeploy

`script/Unwind.s.sol` recovers every asset so the funds can be reused on a new
deployment. Two phases, because the EntryPoint enforces the unstake delay:

```bash
# phase 1: swap booked WLD -> deposit, sweep stray WLD, withdraw deposit, unlock stake
PAYMASTER=0x... RECIPIENT=0x... forge script script/Unwind.s.sol:Unwind \
  --rpc-url "$WORLDCHAIN_RPC_URL" --private-key "$PK" --broadcast

# phase 2: ~1 day later
PAYMASTER=0x... RECIPIENT=0x... ACTION=claim-stake \
  forge script script/Unwind.s.sol:Unwind \
  --rpc-url "$WORLDCHAIN_RPC_URL" --private-key "$PK" --broadcast
```

Owner-only. `SKIP_SWAP=true` skips the batch swap in phase 1.

`sweepExcessWld` deliberately cannot touch `accumulatedWld` — WLD booked for
settlement only exits via `triggerBatchSwap`, which needs the batch window open
and enough pool depth to clear the slippage bound. If phase 1 reports WLD still
booked, re-run it after the window opens; the script is idempotent.

**Set `RECIPIENT` to an address other than the broadcasting EOA when testing on
anvil.** With `RECIPIENT` equal to the sender, anvil reports a bogus sender
balance (a constant 179168 wei regardless of starting funds, unrelated to receipt
gas — every state transition is still correct). With a distinct recipient, anvil
reconciles exactly. `test/Unwind.fork.t.sol` asserts the accounting to the wei.

## How much ETH does it need?

**The EntryPoint enforces no minimum deposit and no minimum stake.** Its only
hard rule is per-op: validation fails (`AA31 paymaster deposit too low`) unless
the deposit covers that op's `maxCost`. The real minimums come from three places:

| Requirement | Amount | Enforced by |
|---|---|---|
| Deposit ≥ op `maxCost` | ~1e-5 ETH per op | EntryPoint, per op |
| Deposit ≥ `maxCost` + `minEntryPointDeposit` | floor is **reserved**, never spent | this paymaster |
| Stake > 0, unstake delay ≥ 86400s | see below | bundler, not the chain |

**Deposit.** World Chain gas is cheap — base fee measured at ~0.0005 gwei, so a
typical ERC-4337 op's `maxCost` lands around **1e-5 ETH** (dominated by the L1
data fee inside `preVerificationGas`, not L2 execution). The script defaults to
**0.02 ETH deposit above a 0.002 ETH floor**, i.e. ~1,800 sponsored ops of
headroom, and it is self-replenishing: `triggerBatchSwap` converts collected WLD
back into deposit. Note the floor is reserved, not spendable — a deposit at or
below `minEntryPointDeposit` sponsors **nothing**, so the script refuses it.

**Stake.** The EntryPoint accepts any non-zero amount, but bundlers reject
under-staked paymasters, and that threshold is *their* config, not the chain's.
The ERC-4337 canonical mempool references ~1 ETH-equivalent with an 86400s (1 day)
unstake delay; private/whitelisted mempools like World Chain's Rundler sidecar are
usually configured lower. The script defaults to **0.05 ETH with a 1-day delay**
and enforces the 1-day minimum — **confirm the actual `min_stake_value` with
whoever runs the bundler before going live**, since too little stake means every
op is rejected with no on-chain error to look at.

Deploying costs ~3.3M gas ≈ **0.0000036 ETH** at current World Chain prices, so
the deposit and stake dominate. Budget **~0.07 ETH** total for the default setup.

## Status of the deliverable

- ✅ Compiles (`solc 0.8.23`) and passes its own Foundry tests.
- ✅ Design doc covering architecture, oracle+premium, collection, batching,
  deposit management, risks, and whitelisting.
- ✅ Chainlink pricing fork-tested against the live World Chain WLD/USD and
  ETH/USD feeds.
- ✅ Full charge → reconcile → swap → re-deposit loop fork-tested against the
  live EntryPoint v0.7, WLD, SwapRouter02 and WLD/WETH pool.
- ✅ Deploy script fork-tested: it deploys, configures, funds, stakes, and the
  result is proven able to sponsor a UserOp.
- ⬜ Not run through a real bundler/smart account yet; bundler stake minimum
  unconfirmed.

### Client integration: `paymasterAndData`

The client MUST supply the 32-byte ceiling on the WLD this op may pull, or
validation reverts `InvalidPaymasterData`:

```
paymasterAndData = paymaster (20B) | verificationGasLimit (16B) | postOpGasLimit (16B) | maxWldAllowed (32B)
```

Build it with `paymaster.encodePaymasterAndData(verificationGas, postOpGas, maxWld)`.
Size `maxWldAllowed` from `quoteWldCharge(maxCost)` plus headroom for oracle drift
before inclusion; if the priced charge exceeds it the op reverts
`WldChargeExceedsMax(required, allowed)` and no WLD is taken. Pass `0` to skip the
check and accept whatever the oracle prices — the field itself is still required.
`postOpGasLimit` must be non-zero or v0.7 skips `postOp` and the user is never
refunded.
- ⬜ **Owner is fully trusted** — no timelock, pause, or guardian; see DESIGN.md §7.
- ⬜ Not audited; no
  permit/Permit2 path yet. See DESIGN.md §7 & §9 for open risks/follow-ups.
