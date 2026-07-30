// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {BasePaymaster} from "@account-abstraction/core/BasePaymaster.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

import {IWldEthOracle} from "./interfaces/IWldEthOracle.sol";
import {ISwapRouter, IWETH9} from "./interfaces/ISwapRouter.sol";

/**
 * @title WLDPaymaster
 * @notice Fully on-chain, backend-less ERC-4337 paymaster that lets users pay
 *         gas in WLD instead of ETH on World Chain.
 *
 * @dev High-level flow (see DESIGN.md for the full write-up):
 *
 *  1. `validatePaymasterUserOp`: prices the op's max ETH cost in WLD via an
 *     {IWldEthOracle} TWAP, adds a `premiumBps` premium (default 20%), and pulls
 *     that maximum WLD charge from the user with `transferFrom`. State writes in
 *     validation are acceptable here because this paymaster is *whitelisted* on
 *     the World Chain Rundler proxy sidecar (see design doc), so it is exempt
 *     from ERC-4337 storage-rule reputation checks.
 *
 *  2. `postOp`: computes the actual WLD charge pro-rata to actual gas used,
 *     refunds the difference to the user, and books the charge into
 *     `accumulatedWld`.
 *
 *  3. `triggerBatchSwap`: permissionless; once `blocksPerBatch` blocks have
 *     elapsed, swaps the accumulated WLD to ETH via Uniswap V3 (with oracle-
 *     bounded slippage protection), unwraps WETH, and re-deposits the ETH into
 *     the EntryPoint — making the paymaster self-sustaining after initial funding.
 *
 * SECURITY: This is a POC/MVP. It has NOT been audited. Paymasters custody funds
 * and are high-value targets — do not deploy to production without review.
 */
contract WLDPaymaster is BasePaymaster, ReentrancyGuard {
    using SafeERC20 for IERC20;

    uint256 internal constant BPS = 10_000;

    // --- immutable config ---
    IERC20 public immutable wld;
    IWETH9 public immutable weth;
    ISwapRouter public immutable swapRouter;

    // --- owner-configurable config ---
    /// @notice Price oracle (WLD/ETH). Swappable (TWAP now, Chainlink later).
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

    /// @dev Context passed from validate -> postOp.
    struct Context {
        address sender;
        uint256 maxWldCharge;
        uint256 maxCost;
    }

    constructor(
        IEntryPoint _entryPoint,
        IERC20 _wld,
        IWETH9 _weth,
        ISwapRouter _swapRouter,
        IWldEthOracle _oracle,
        uint24 _swapPoolFee
    ) BasePaymaster(_entryPoint) {
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
        lastBatchBlock = block.number;
    }

    receive() external payable {}

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
        if (getDeposit() < maxCost + minEntryPointDeposit) revert DepositFloorBreached();

        uint256 maxWldCharge = _wldCharge(maxCost);

        if (wld.balanceOf(sender) < maxWldCharge) revert InsufficientWldBalance();
        if (wld.allowance(sender, address(this)) < maxWldCharge) revert InsufficientWldAllowance();

        wld.safeTransferFrom(sender, address(this), maxWldCharge);

        context = abi.encode(Context({sender: sender, maxWldCharge: maxWldCharge, maxCost: maxCost}));
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
        if (costWithPostOp > ctx.maxCost) costWithPostOp = ctx.maxCost;

        // Pro-rata the WLD charge (premium is already baked into maxWldCharge).
        uint256 actualWldCharge = ctx.maxCost == 0 ? 0 : (ctx.maxWldCharge * costWithPostOp) / ctx.maxCost;
        uint256 refund = ctx.maxWldCharge - actualWldCharge;

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

    // =========================================================================
    //                          Batched settlement
    // =========================================================================

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

        // checks-effects-interactions: reset accounting before external calls
        accumulatedWld = 0;
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
                deadline: block.timestamp,
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
        entryPoint.depositTo{value: redeposit}(address(this));

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
