// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.26;

import {Test, console2} from "forge-std/Test.sol";
import {FeeEscrow, IChainLinkPriceFeed, IBurnCallback} from "../src/fees/FeeEscrow.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

interface ISwapRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

interface IWETH {
    function deposit() external payable;
    function approve(address spender, uint256 amount) external returns (bool);
}

interface IUniswapV3Pool {
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);

    function token0() external view returns (address);
    function token1() external view returns (address);
}

/// @notice Real swap executor using Uniswap V3 pool directly
contract UniswapV3BurnExecutor is IBurnCallback {
    IERC20 public immutable wld;
    IWETH public immutable weth;
    IUniswapV3Pool public immutable pool;

    // Min/max sqrt price limits for swaps
    uint160 internal constant MIN_SQRT_RATIO = 4295128739;
    uint160 internal constant MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342;

    constructor(address _wld, address _weth, address _pool) {
        wld = IERC20(_wld);
        weth = IWETH(_weth);
        pool = IUniswapV3Pool(_pool);
    }

    function burnCallback(uint256 expectedBurn, address recipient) external payable override {
        // Wrap ETH to WETH
        weth.deposit{value: msg.value}();

        // Determine swap direction
        address token0 = pool.token0();
        bool zeroForOne = token0 == address(weth); // true if WETH is token0

        // Swap WETH -> WLD via V3 pool
        uint160 sqrtPriceLimitX96 = zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1;

        pool.swap(
            recipient, // recipient gets the WLD
            zeroForOne, // direction
            int256(msg.value), // exact input amount
            sqrtPriceLimitX96,
            abi.encode(expectedBurn)
        );

        IERC20(wld).transfer(recipient, wld.balanceOf(address(this)));
    }

    /// @notice Uniswap V3 callback - pay the pool
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata) external {
        require(msg.sender == address(pool), "Invalid callback");
        uint256 expectedBurn = abi.decode(msg.data, (uint256));

        // Pay the positive delta (what we owe)
        if (amount0Delta > 0) {
            IERC20(pool.token0()).transfer(msg.sender, uint256(amount0Delta));
            if (uint256(amount1Delta) < expectedBurn) {
                revert("Insufficient burn here");
            }
        }
        if (amount1Delta > 0) {
            IERC20(pool.token1()).transfer(msg.sender, uint256(amount1Delta));
            if (uint256(amount0Delta) < expectedBurn) {
                revert("Insufficient burn here");
            }
        }
    }
}

contract FeeEscrowHarness is FeeEscrow {
    constructor(address owner_, uint256 minimumInterval_) FeeEscrow(owner_, minimumInterval_) {}

    function exposedPrice() external view returns (uint128) {
        return price();
    }
}

contract TestFeeEscrow is Test {
    // World Chain Mainnet addresses
    address constant WETH = 0x4200000000000000000000000000000000000006;
    address constant WLD = 0x2cFc85d8E48F8EAB294be644d9E25C3030863003;

    // Oracle addresses
    address constant WLD_USD_ORACLE = 0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0;
    address constant ETH_USD_ORACLE = 0xe1d72a719171DceAB9499757EB9d5AEb9e8D64A6;

    FeeEscrowHarness escrow;
    UniswapV3BurnExecutor burnExecutor;

    address owner;

    uint256 constant MINIMUM_INTERVAL = 1 hours;

    function setUp() public {
        string memory rpcUrl = vm.envString("WORLDCHAIN_PROVIDER");
        vm.createSelectFork(rpcUrl);

        owner = makeAddr("owner");

        escrow = new FeeEscrowHarness(owner, MINIMUM_INTERVAL);

        burnExecutor = new UniswapV3BurnExecutor(WLD, WETH, 0x494D68e3cAb640fa50F4c1B3E2499698D1a173A0);

        vm.label(address(escrow), "FeeEscrow");
        vm.label(address(burnExecutor), "BurnExecutor");
    }

    function test_BurnBeforeInterval_RevertsBurnTooSoon() public {
        deal(address(escrow), 1 ether);

        // Immediately after deployment, should fail
        vm.prank(address(burnExecutor));
        vm.expectRevert(FeeEscrow.BurnTooSoon.selector);
        escrow.executeBurn();
    }

    function test_ExpiredPrice_Reverts_StalePrice() public {
        deal(address(escrow), 1 ether);
        vm.warp(block.timestamp + MINIMUM_INTERVAL + 1);

        // Valid ETH price
        vm.mockCall(
            ETH_USD_ORACLE,
            abi.encodeWithSelector(IChainLinkPriceFeed.priceFeed.selector),
            abi.encode(
                IChainLinkPriceFeed.PriceFeedData({
                    price: int192(2500e8),
                    timestamp: uint32(block.timestamp),
                    expiresAt: uint32(block.timestamp + 1 hours)
                })
            )
        );
        // Expired WLD price
        vm.mockCall(
            WLD_USD_ORACLE,
            abi.encodeWithSelector(IChainLinkPriceFeed.priceFeed.selector),
            abi.encode(
                IChainLinkPriceFeed.PriceFeedData({
                    price: int192(0.5e8),
                    timestamp: uint32(block.timestamp - 2 hours),
                    expiresAt: uint32(block.timestamp - 1)
                })
            )
        );

        vm.prank(address(burnExecutor));
        vm.expectRevert(FeeEscrow.StalePrice.selector);
        escrow.executeBurn();
    }

    function test_BurnInvalid_RevertsInsufficientBurn() public {
        deal(address(escrow), 1 ether);
        vm.warp(block.timestamp + MINIMUM_INTERVAL + 1);

        deal(WLD, address(burnExecutor), 0);
        vm.prank(address(burnExecutor));
        vm.expectPartialRevert(FeeEscrow.InsufficientBurn.selector);
        escrow.executeBurn();
    }

    function test_BurnReentrancy_Reverts() public {
        vm.deal(address(escrow), 1 ether);
        vm.warp(block.timestamp + MINIMUM_INTERVAL + 1);
        vm.prank(address(this));
        vm.expectRevert(ReentrancyGuardTransient.ReentrancyGuardReentrantCall.selector);
        escrow.executeBurn();
    }

    function burnCallback(uint256, address) external payable {
        // Re-enter executeBurn during callback
        escrow.executeBurn();
    }

    function test_Burn(uint128 amount) public {
        uint128 price = escrow.exposedPrice();
        console2.logUint(price);
        vm.assume(amount > 1e15 && amount < 10 ether);

        deal(address(escrow), amount);
        deal(WLD, address(burnExecutor), type(uint128).max);

        // 2. Wait for interval
        vm.warp(block.timestamp + MINIMUM_INTERVAL + 1);

        uint256 recipientWldBefore = IERC20(WLD).balanceOf(escrow.DEAD_ADDRESS());
        uint256 timestampBefore = escrow.lastBuybackTimestamp();

        // 4. Execute burn
        vm.prank(address(burnExecutor));
        escrow.executeBurn();

        // 5. Verify all invariants held
        assertEq(address(escrow).balance, 0, "All ETH should be spent");
        assertGt(IERC20(WLD).balanceOf(escrow.DEAD_ADDRESS()), recipientWldBefore, "Recipient should receive WLD");
        assertGt(escrow.lastBuybackTimestamp(), timestampBefore, "Timestamp should update");
    }

    function test_Price() public view {
        uint128 price = escrow.exposedPrice();
        console2.logUint(price);
        assertGe(price, 0, "Price should be non-negative");
    }

    function test_SetInterval_RevertsIfNotOwner(address _naughty) public {
        vm.prank(_naughty);
        vm.expectPartialRevert(Ownable.OwnableUnauthorizedAccount.selector);
        escrow.setInterval(2 hours);
    }

    function test_Withdraw_RevertsIfNotOwner(address _naughty) public {
        vm.prank(_naughty);
        vm.expectPartialRevert(Ownable.OwnableUnauthorizedAccount.selector);
        escrow.withdraw(address(0), 0);
    }

    function test_WithdrawToken_RevertsIfNotOwner(address _naughty) public {
        vm.prank(_naughty);
        vm.expectPartialRevert(Ownable.OwnableUnauthorizedAccount.selector);
        escrow.withdraw(WLD, address(0), 100);
    }

    receive() external payable {}
}
