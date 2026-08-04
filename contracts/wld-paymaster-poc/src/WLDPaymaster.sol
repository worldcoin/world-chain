// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuardUpgradeable} from "@openzeppelin/contracts-upgradeable/utils/ReentrancyGuardUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";

import {BasePaymasterUpgradeable} from "./BasePaymasterUpgradeable.sol";

import {IWldEthOracle} from "./interfaces/IWldEthOracle.sol";
import {ISwapRouter, IWETH9} from "./interfaces/ISwapRouter.sol";

/**
 * @title WLDPaymaster
 * @notice Fully on-chain, backend-less ERC-4337 paymaster that lets users pay
 *         gas in WLD instead of ETH on World Chain.
 *
 * @dev High-level flow (see DESIGN.md for the full write-up):
 *
 *  1. `validatePaymasterUserOp`: prices the op's max ETH cost (`maxCost`, the
 *     EntryPoint's estimate) in WLD via {IWldEthOracle} (Chainlink WLD/USD x
 *     ETH/USD by default), adds a `premiumBps` premium (default 20%), and pulls
 *     that maximum WLD charge from the user with `transferFrom`.
 *
 *     A client can cap its exposure by appending its own ceiling —
 *     `abi.encode(maxWldAllowed)`, 32 bytes — as `paymasterData` (i.e.
 *     `paymasterAndData[52:]`; bytes 20..52 are the gas limits the EntryPoint
 *     itself parses). If the priced charge exceeds that ceiling the op reverts with
 *     {WldChargeExceedsMax}, so a bad oracle print or a premium raised between
 *     quote and inclusion can never pull more WLD than the user signed off on.
 *
 *     The field is optional: omitted, or an explicit 0, means no ceiling. Any other
 *     length is a client bug and reverts {InvalidPaymasterData}.
 *
 *     The WLD/ETH rate actually used is carried to `postOp` in the context, so the
 *     refund is settled at the same price the charge was taken at — a mid-op
 *     oracle move cannot change what the user pays.
 *
 *     Writing token state during validation touches the paymaster's *own*
 *     associated storage in the WLD contract, which ERC-7562 only permits for a
 *     **staked** entity. This paymaster therefore MUST call `addStake(...)` on the
 *     EntryPoint before any bundler will accept its ops (whitelisting on the World
 *     Chain Rundler sidecar covers reputation, not the storage rules). A deployment
 *     that can neither stake nor be whitelisted must instead only *check*
 *     balance/allowance here and pull in postOp.
 *
 *  2. `postOp`: computes the actual WLD charge pro-rata to actual gas used,
 *     refunds the difference to the user, and books the charge into
 *     `accumulatedWld`.
 *
 *  3. `triggerBatchSwap`: permissionless; once `blocksPerBatch` blocks have
 *     elapsed, swaps up to `maxWldPerBatch` of the accumulated WLD to ETH via
 *     Uniswap V3 SwapRouter02 (with oracle-bounded slippage protection), unwraps
 *     WETH, and re-deposits the ETH into the EntryPoint — making the paymaster
 *     self-sustaining after initial funding.
 *
 * UPGRADEABILITY: deployed behind an ERC-1967 proxy (UUPS). The proxy address is
 * what users approve WLD to and what clients put in `paymasterAndData`, so it stays
 * fixed across upgrades. `_authorizeUpgrade` is `onlyOwner`, which means the owner
 * can replace validation logic outright — a strictly larger power than the drain
 * and oracle-swap it already has. Hold ownership in a multisig, ideally timelocked.
 *
 * SECURITY: This is a POC/MVP. It has NOT been audited. Paymasters custody funds
 * and are high-value targets — do not deploy to production without review.
 */
contract WLDPaymaster is BasePaymasterUpgradeable, ReentrancyGuardUpgradeable, UUPSUpgradeable {
    using SafeERC20 for IERC20;

    uint256 internal constant BPS = 10_000;
    /// @dev Fixed-point scale for the WLD-per-wei rate carried in {Context}.
    uint256 internal constant RATE_SCALE = 1e18;
    /// @dev `PAYMASTER_DATA_OFFSET` (52 = 20-byte paymaster + 16-byte
    ///      verificationGasLimit + 16-byte postOpGasLimit) comes from BasePaymaster.
    /// @dev Byte length of `paymasterData` when the optional ceiling is present.
    uint256 internal constant PAYMASTER_DATA_LENGTH = 32;

    // --- set once at initialization ---
    // Not `immutable`: an implementation's immutables live in its own bytecode, so
    // behind a proxy they would have to be re-supplied on every upgrade and could
    // silently diverge from the storage the proxy actually uses.
    IERC20 public wld;
    IWETH9 public weth;
    ISwapRouter public swapRouter;

    // --- owner-configurable config ---
    /// @notice Price oracle (WLD/ETH). Swappable: Chainlink default, TWAP fallback.
    IWldEthOracle public oracle;
    /// @notice Premium charged over oracle price, in bps (2000 = +20%).
    uint256 public premiumBps;
    /// @notice Minimum blocks between batch swaps.
    uint256 public blocksPerBatch;
    /// @notice Max acceptable slippage on the batch swap vs oracle price, in bps.
    uint256 public maxSwapSlippageBps;
    /// @notice Uniswap V3 fee tier for the WLD/WETH swap pool.
    uint24 public swapPoolFee;
    /// @notice EntryPoint deposit floor that must remain *after* sponsoring an op.
    uint256 public minEntryPointDeposit;
    /// @notice Gas assumed for `postOp`, added to the charge so postOp is covered.
    uint256 public postOpGasOverhead;
    /// @notice Optional reward paid to `triggerBatchSwap` caller, in bps of ETH proceeds.
    uint256 public keeperRewardBps;
    /// @notice Max WLD swapped per batch (0 = unlimited). Bounds price impact.
    uint256 public maxWldPerBatch;

    // --- batch accounting ---
    /// @notice WLD collected from users, awaiting the next batch swap.
    uint256 public accumulatedWld;
    /// @notice Block number of the last executed batch swap.
    uint256 public lastBatchBlock;

    // --- events ---
    event GasCharged(address indexed sender, uint256 wldCharged, uint256 actualGasCost);
    event BatchSwapExecuted(
        address indexed caller, uint256 wldIn, uint256 ethOut, uint256 keeperReward, uint256 redeposited
    );
    event ConfigUpdated();
    event OracleUpdated(address indexed oracle);

    // --- errors ---
    error InsufficientWldBalance();
    error InsufficientWldAllowance();
    error DepositFloorBreached();
    error BatchTooEarly();
    error NothingToSwap();
    error SlippageTooHigh();
    error InvalidConfig();
    /// @param length Byte length of the supplied `paymasterData` (must be 0 or 32).
    error InvalidPaymasterData(uint256 length);
    /// @param required WLD the op would take at the current oracle price + premium.
    /// @param allowed Non-zero ceiling the client encoded in `paymasterData`.
    error WldChargeExceedsMax(uint256 required, uint256 allowed);

    /// @dev Context passed from validate -> postOp.
    /// @param wldTaken WLD actually pulled from the sender during validation.
    /// @param wldPerWeiRate WLD per wei of gas cost, scaled by {RATE_SCALE}. Frozen
    ///        at validation time so postOp refunds at the price charged, not a
    ///        fresh oracle read.
    struct Context {
        address sender;
        uint256 wldTaken;
        uint256 wldPerWeiRate;
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        // The implementation must never be initialized in its own right: an
        // initialized implementation with an owner is a live paymaster that could be
        // made to selfdestruct-equivalent (upgraded) out from under nobody, and it
        // muddies which address holds the real state.
        _disableInitializers();
    }

    /**
     * @notice Initializes the proxy. Callable exactly once.
     * @param initialOwner Owner: can configure, drain the deposit, swap the oracle,
     *        and upgrade the implementation. Use a multisig.
     */
    function initialize(
        IEntryPoint _entryPoint,
        IERC20 _wld,
        IWETH9 _weth,
        ISwapRouter _swapRouter,
        IWldEthOracle _oracle,
        uint24 _swapPoolFee,
        address initialOwner
    ) external initializer {
        if (
            address(_wld) == address(0) || address(_weth) == address(0) || address(_swapRouter) == address(0)
                || address(_oracle) == address(0)
        ) revert InvalidConfig();

        __BasePaymaster_init(_entryPoint, initialOwner);
        __ReentrancyGuard_init();
        __UUPSUpgradeable_init();

        wld = _wld;
        weth = _weth;
        swapRouter = _swapRouter;
        oracle = _oracle;
        swapPoolFee = _swapPoolFee;

        // sensible defaults
        premiumBps = 2_000; // +20%
        blocksPerBatch = 300; // ~10 min at 2s blocks
        maxSwapSlippageBps = 300; // 3%
        minEntryPointDeposit = 0.05 ether;
        postOpGasOverhead = 40_000;
        keeperRewardBps = 0;
        // Conservative default: the World Chain WLD/WETH 0.3% pool is shallow on
        // the WETH side, so keep single-batch price impact well inside
        // `maxSwapSlippageBps`. Tune with `setMaxWldPerBatch` against live depth.
        maxWldPerBatch = 500e18;
        lastBatchBlock = block.number;
    }

    receive() external payable {}

    /// @dev UUPS: only the owner may ship a new implementation.
    function _authorizeUpgrade(address) internal override onlyOwner {}

    /**
     * @notice Layout version of this implementation, bumped whenever storage
     *         changes, so a deployment can be checked against the ABI expected of it.
     */
    function version() external pure virtual returns (string memory) {
        return "2.0.0";
    }

    // =========================================================================
    //                          ERC-4337 paymaster hooks
    // =========================================================================

    /**
     * @dev Charges the user the *maximum* possible WLD cost up-front (oracle
     *      price + premium) and returns the data needed to reconcile in postOp.
     *      Pulling funds during validation is safe here because the paymaster is
     *      whitelisted by the bundler; a non-whitelisted deployment MUST instead
     *      only *check* balance/allowance here and pull in postOp.
     */
    function _validatePaymasterUserOp(PackedUserOperation calldata userOp, bytes32, uint256 maxCost)
        internal
        override
        returns (bytes memory context, uint256 validationData)
    {
        address sender = userOp.sender;

        // Keep a safety buffer in the EntryPoint so a burst of ops can't drain
        // the deposit below the floor mid-batch.
        //
        // `getDeposit()` is already NET of this op: EntryPoint v0.7 does
        // `paymasterInfo.deposit -= requiredPreFund` *before* calling us (see
        // EntryPoint._validatePaymasterPrepayment). So the remaining balance is
        // exactly what would be left after sponsoring, and `maxCost` must NOT be
        // subtracted again — doing so demanded ~`2 * maxCost + floor` and rejected
        // ops that left the floor fully intact. Covered by
        // test/EntryPointIntegration.t.sol, which drives real `handleOps`.
        if (getDeposit() < minEntryPointDeposit) revert DepositFloorBreached();

        // Price the EntryPoint's own worst-case estimate for this op.
        uint256 maxWldCharge = _wldCharge(maxCost);

        // Optional client-signed ceiling: omitted or 0 means the client accepts
        // whatever the oracle prices. A wrong-length payload still reverts rather
        // than being reinterpreted.
        uint256 maxWldAllowed = _decodeMaxWldAllowed(userOp.paymasterAndData);
        if (maxWldAllowed != 0 && maxWldCharge > maxWldAllowed) {
            revert WldChargeExceedsMax(maxWldCharge, maxWldAllowed);
        }

        if (wld.balanceOf(sender) < maxWldCharge) revert InsufficientWldBalance();
        if (wld.allowance(sender, address(this)) < maxWldCharge) revert InsufficientWldAllowance();

        wld.safeTransferFrom(sender, address(this), maxWldCharge);

        // Freeze the effective rate (WLD per wei, premium included) for postOp.
        uint256 rate = maxCost == 0 ? 0 : (maxWldCharge * RATE_SCALE) / maxCost;

        context = abi.encode(Context({sender: sender, wldTaken: maxWldCharge, wldPerWeiRate: rate}));
        validationData = 0; // valid, no time range
    }

    /**
     * @dev Reconciles the up-front charge against actual gas used, refunds the
     *      excess WLD to the user, and books the net charge for batching.
     */
    function _postOp(PostOpMode, bytes calldata context, uint256 actualGasCost, uint256 actualUserOpFeePerGas)
        internal
        override
    {
        Context memory ctx = abi.decode(context, (Context));

        // Include an estimate of this postOp's own gas so it is covered by WLD.
        uint256 costWithPostOp = actualGasCost + postOpGasOverhead * actualUserOpFeePerGas;

        // Re-use the rate frozen at validation time — never a fresh oracle read —
        // so the refund is priced exactly as the charge was.
        uint256 actualWldCharge = (ctx.wldPerWeiRate * costWithPostOp) / RATE_SCALE;
        // Actual cost can exceed the estimate (or rounding can nudge it up); the
        // user never owes more than was taken.
        if (actualWldCharge > ctx.wldTaken) actualWldCharge = ctx.wldTaken;
        uint256 refund = ctx.wldTaken - actualWldCharge;

        accumulatedWld += actualWldCharge;
        if (refund > 0) wld.safeTransfer(ctx.sender, refund);

        emit GasCharged(ctx.sender, actualWldCharge, actualGasCost);
    }

    /// @notice WLD to charge for `ethWei` of gas, including the premium.
    function _wldCharge(uint256 ethWei) internal view returns (uint256) {
        uint256 base = oracle.wldForEth(ethWei);
        return (base * (BPS + premiumBps)) / BPS;
    }

    /// @notice View helper: quote the WLD charge (incl. premium) for an ETH cost.
    function quoteWldCharge(uint256 ethWei) external view returns (uint256) {
        return _wldCharge(ethWei);
    }

    /**
     * @dev Reads the client's WLD ceiling out of `paymasterAndData`. `paymasterData`
     *      is either empty (no ceiling) or exactly 32 bytes; any other length is a
     *      client bug and must not be silently reinterpreted.
     */
    function _decodeMaxWldAllowed(bytes calldata paymasterAndData) internal pure returns (uint256) {
        uint256 length = paymasterAndData.length;
        // Guard the subtraction: a direct caller can pass anything, and an
        // arithmetic panic here is a much worse error message than the named one.
        uint256 dataLength = length < PAYMASTER_DATA_OFFSET ? 0 : length - PAYMASTER_DATA_OFFSET;
        // Omitted entirely == 0 == no ceiling.
        if (dataLength == 0) return 0;
        if (dataLength != PAYMASTER_DATA_LENGTH) revert InvalidPaymasterData(dataLength);
        return uint256(bytes32(paymasterAndData[PAYMASTER_DATA_OFFSET:]));
    }

    /**
     * @notice Client helper: build the full `paymasterAndData` field for a UserOp.
     * @param maxWldAllowed Ceiling on WLD this op may pull. Quote it with
     *        {quoteWldCharge} and add headroom for oracle drift before inclusion.
     *        0 omits the field entirely, accepting whatever the oracle prices.
     * @dev The gas limits are not the paymaster's own convention — the EntryPoint
     *      unpacks them from these exact offsets. `postOpGasLimit` must be non-zero
     *      or v0.7 skips `postOp` and no refund is issued: the user then eats the
     *      full max charge.
     */
    function encodePaymasterAndData(uint128 verificationGasLimit, uint128 postOpGasLimit, uint256 maxWldAllowed)
        external
        view
        returns (bytes memory)
    {
        if (maxWldAllowed == 0) {
            return abi.encodePacked(address(this), verificationGasLimit, postOpGasLimit);
        }
        return abi.encodePacked(address(this), verificationGasLimit, postOpGasLimit, maxWldAllowed);
    }

    // =========================================================================
    //                          Batched settlement
    // =========================================================================

    /// @notice WLD that the next `triggerBatchSwap` would sell (batch cap applied).
    function nextBatchAmount() public view returns (uint256) {
        uint256 amount = accumulatedWld;
        if (maxWldPerBatch != 0 && amount > maxWldPerBatch) amount = maxWldPerBatch;
        return amount;
    }

    /// @notice True once enough blocks have elapsed and there is WLD to swap.
    function batchReady() public view returns (bool) {
        return block.number >= lastBatchBlock + blocksPerBatch && accumulatedWld > 0;
    }

    /**
     * @notice Permissionless: swap accumulated WLD -> ETH and re-deposit to the
     *         EntryPoint. Callable by anyone once `blocksPerBatch` have elapsed.
     * @dev No off-chain keeper is required — any account (including the next
     *      UserOp's bundler, a searcher chasing `keeperRewardBps`, or the owner)
     *      can crank it. Slippage is bounded by the oracle price.
     * @param minEthOut Caller-supplied floor on ETH received; the effective floor
     *        is `max(minEthOut, oraclePrice * (1 - maxSwapSlippageBps))`.
     */
    function triggerBatchSwap(uint256 minEthOut) external nonReentrant returns (uint256 ethOut) {
        if (block.number < lastBatchBlock + blocksPerBatch) revert BatchTooEarly();

        uint256 amountIn = accumulatedWld;
        if (amountIn == 0) revert NothingToSwap();
        // Cap the batch so a large backlog can't exceed what the pool absorbs
        // inside `maxSwapSlippageBps` — otherwise the swap would revert on every
        // attempt and settlement would stall permanently. The remainder stays
        // accumulated and drains over subsequent batches.
        if (maxWldPerBatch != 0 && amountIn > maxWldPerBatch) amountIn = maxWldPerBatch;

        // checks-effects-interactions: reset accounting before external calls
        accumulatedWld -= amountIn;
        lastBatchBlock = block.number;

        // Oracle-bounded minimum out (slippage protection).
        uint256 oracleEth = oracle.ethForWld(amountIn);
        uint256 oracleFloor = (oracleEth * (BPS - maxSwapSlippageBps)) / BPS;
        uint256 floor = minEthOut > oracleFloor ? minEthOut : oracleFloor;

        // Swap WLD -> WETH into this contract.
        wld.forceApprove(address(swapRouter), amountIn);
        ethOut = swapRouter.exactInputSingle(
            ISwapRouter.ExactInputSingleParams({
                tokenIn: address(wld),
                tokenOut: address(weth),
                fee: swapPoolFee,
                recipient: address(this),
                amountIn: amountIn,
                amountOutMinimum: floor,
                sqrtPriceLimitX96: 0
            })
        );
        if (ethOut < floor) revert SlippageTooHigh();

        // Unwrap WETH -> ETH.
        weth.withdraw(ethOut);

        // Optional keeper incentive.
        uint256 keeperReward;
        if (keeperRewardBps > 0) {
            keeperReward = (ethOut * keeperRewardBps) / BPS;
            if (keeperReward > 0) {
                (bool ok,) = payable(msg.sender).call{value: keeperReward}("");
                require(ok, "keeper xfer failed");
            }
        }

        // Re-deposit remaining ETH into the EntryPoint to self-replenish.
        uint256 redeposit = ethOut - keeperReward;
        entryPoint().depositTo{value: redeposit}(address(this));

        emit BatchSwapExecuted(msg.sender, amountIn, ethOut, keeperReward, redeposit);
    }

    // =========================================================================
    //                          Owner configuration
    // =========================================================================

    function setOracle(IWldEthOracle _oracle) external onlyOwner {
        if (address(_oracle) == address(0)) revert InvalidConfig();
        oracle = _oracle;
        emit OracleUpdated(address(_oracle));
    }

    function setPremiumBps(uint256 _premiumBps) external onlyOwner {
        if (_premiumBps > BPS) revert InvalidConfig(); // cap premium at +100%
        premiumBps = _premiumBps;
        emit ConfigUpdated();
    }

    function setBlocksPerBatch(uint256 _blocksPerBatch) external onlyOwner {
        if (_blocksPerBatch == 0) revert InvalidConfig();
        blocksPerBatch = _blocksPerBatch;
        emit ConfigUpdated();
    }

    function setMaxSwapSlippageBps(uint256 _bps) external onlyOwner {
        if (_bps >= BPS) revert InvalidConfig();
        maxSwapSlippageBps = _bps;
        emit ConfigUpdated();
    }

    function setSwapPoolFee(uint24 _fee) external onlyOwner {
        swapPoolFee = _fee;
        emit ConfigUpdated();
    }

    function setMinEntryPointDeposit(uint256 _min) external onlyOwner {
        minEntryPointDeposit = _min;
        emit ConfigUpdated();
    }

    function setPostOpGasOverhead(uint256 _overhead) external onlyOwner {
        postOpGasOverhead = _overhead;
        emit ConfigUpdated();
    }

    /// @param _max Max WLD per batch swap; 0 disables the cap (not recommended).
    function setMaxWldPerBatch(uint256 _max) external onlyOwner {
        maxWldPerBatch = _max;
        emit ConfigUpdated();
    }

    function setKeeperRewardBps(uint256 _bps) external onlyOwner {
        if (_bps > BPS) revert InvalidConfig();
        keeperRewardBps = _bps;
        emit ConfigUpdated();
    }

    /// @notice Rescue WLD that is not owed to the batch (e.g. donated tokens).
    /// @dev Cannot touch `accumulatedWld`, which belongs to the settlement flow.
    function sweepExcessWld(address to) external onlyOwner {
        uint256 bal = wld.balanceOf(address(this));
        uint256 excess = bal > accumulatedWld ? bal - accumulatedWld : 0;
        if (excess > 0) wld.safeTransfer(to, excess);
    }
}
