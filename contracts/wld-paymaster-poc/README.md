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
  E2E.fork.t.sol                   # optional: full loop vs live EntryPoint/WLD/router/pool
  UniswapV3TwapOracle.t.sol        # TWAP conversion via a mock V3 pool
  mocks/Mocks.sol                  # ERC20 / WETH / oracle / aggregator / router / pool mocks
script/Deploy.s.sol               # example deployment wiring
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

Expected: **40 passing unit tests**, plus **4 fork tests** that are skipped unless
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
| `minEntryPointDeposit` | 0.05 ETH | Deposit floor preserved after each op |
| `postOpGasOverhead` | 40000 | Gas assumed for postOp, folded into the charge |
| `keeperRewardBps` | 0 | Optional reward paid to the batch-swap caller |
| `maxWldPerBatch` | 500 WLD | Max WLD sold per batch; bounds price impact (0 = unlimited) |
| `oracle` | ctor | Swappable `IWldEthOracle` (Chainlink default) |
| `maxStaleness` | 1 hour (oracle ctor) | Max feed answer age before ops are rejected |

## Deployment notes

1. Deploy `ChainlinkWldEthOracle(wldUsdFeed, ethUsdFeed, maxStaleness)` — or set
   `ORACLE_KIND=twap` in `script/Deploy.s.sol` to use `UniswapV3TwapOracle`
   against the WLD/WETH V3 pool instead.
2. Deploy `WLDPaymaster(entryPoint, wld, weth, swapRouter, oracle, poolFee)`.
3. `deposit{value: ...}()` once to seed the EntryPoint balance.
4. **`addStake(...)` — required, not optional.** `validatePaymasterUserOp` writes
   the paymaster's own associated storage in the WLD contract, which ERC-7562
   permits only for a **staked** entity. An unstaked paymaster has its ops
   rejected by standards-compliant bundlers no matter what is whitelisted. Pass
   `STAKE=<wei>` to the deploy script; it warns when you don't.
5. **Whitelist** the paymaster on World Chain's Rundler proxy sidecar (and,
   temporarily, on Alchemy/Pimlico) — that covers reputation, *not* the storage
   rules. See DESIGN.md §8.
6. Tune `maxWldPerBatch` against live pool depth, and confirm the client sets a
   non-zero `paymasterPostOpGasLimit` in `paymasterAndData` — EntryPoint v0.7
   skips `postOp` (so no refund, and no WLD booked for batching) if it is 0.

One-liner (World Chain mainnet defaults, dry run — drop `--broadcast` off/on):

```bash
INITIAL_DEPOSIT=100000000000000000 STAKE=10000000000000000 \
  forge script script/Deploy.s.sol:Deploy --rpc-url "$WORLDCHAIN_RPC_URL" -vvv
```

## Status of the deliverable

- ✅ Compiles (`solc 0.8.23`) and passes its own Foundry tests.
- ✅ Design doc covering architecture, oracle+premium, collection, batching,
  deposit management, risks, and whitelisting.
- ✅ Chainlink pricing fork-tested against the live World Chain WLD/USD and
  ETH/USD feeds.
- ✅ Full charge → reconcile → swap → re-deposit loop fork-tested against the
  live EntryPoint v0.7, WLD, SwapRouter02 and WLD/WETH pool.
- ⬜ Not run through a real bundler/smart account yet (no staked deployment, no
  `paymasterAndData` client path).
- ⬜ **Owner is fully trusted** — no timelock, pause, or guardian; see DESIGN.md §7.
- ⬜ Not audited; no
  permit/Permit2 path yet. See DESIGN.md §7 & §9 for open risks/follow-ups.
