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
  **permissionless** `triggerBatchSwap()` callable every `X` blocks, swaps WLD→ETH
  on Uniswap V3 (with oracle-bounded slippage protection) and **re-deposits the
  ETH into the EntryPoint** — self-sustaining after a one-time funding.

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

# optional: sanity-check the live World Chain Chainlink feeds
WORLDCHAIN_RPC_URL=https://worldchain-mainnet.g.alchemy.com/public \
  forge test --match-contract ChainlinkWldEthOracleForkTest -vv
```

Expected: **36 passing tests** (see `test/`), plus 1 fork test that is skipped
unless `WORLDCHAIN_RPC_URL` is set.

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
| `oracle` | ctor | Swappable `IWldEthOracle` (Chainlink default) |
| `maxStaleness` | 1 hour (oracle ctor) | Max feed answer age before ops are rejected |

## Deployment notes

1. Deploy `ChainlinkWldEthOracle(wldUsdFeed, ethUsdFeed, maxStaleness)` — or set
   `ORACLE_KIND=twap` in `script/Deploy.s.sol` to use `UniswapV3TwapOracle`
   against the WLD/WETH V3 pool instead.
2. Deploy `WLDPaymaster(entryPoint, wld, weth, swapRouter, oracle, poolFee)`.
3. `deposit{value: ...}()` once to seed the EntryPoint balance.
4. **Whitelist** the paymaster on World Chain's Rundler proxy sidecar (and,
   temporarily, on Alchemy/Pimlico). See DESIGN.md §8.

## Status of the deliverable

- ✅ Compiles (`solc 0.8.23`) and passes its own Foundry tests.
- ✅ Design doc covering architecture, oracle+premium, collection, batching,
  deposit management, risks, and whitelisting.
- ✅ Chainlink pricing fork-tested against the live World Chain WLD/USD and
  ETH/USD feeds.
- ⬜ Not audited; paymaster/swap paths not fork-tested against live World Chain
  contracts; no
  permit/Permit2 path yet. See DESIGN.md §7 & §9 for open risks/follow-ups.
