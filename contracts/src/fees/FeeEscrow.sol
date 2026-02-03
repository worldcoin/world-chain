// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

interface IBurnCallback {
    function burnCallback(uint256 expectedBurn, address recipient) external payable;
}

interface IChainLinkPriceFeed {
    struct PriceFeedData {
        int192 price;
        uint32 timestamp;
        uint32 expiresAt;
    }

    function latestRoundData()
        external
        view
        returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);

    function priceFeed() external view returns (PriceFeedData memory);

    function decimals() external view returns (uint8);
}

/// @title FeeEscrow
/// @author Worldcoin
/// @notice Concrete implementation of BuybackExecutor for World Chain fee burns
/// @dev Provides all config specifics: token addresses, recipient, interval, owner
contract FeeEscrow is ReentrancyGuardTransient, Ownable {
    using SafeERC20 for IERC20;

    /*//////////////////////////////////////////////////////////////
                               CONSTANTS
    //////////////////////////////////////////////////////////////*/

    /// @notice WLD contract address
    address public constant WLD = 0x2cFc85d8E48F8EAB294be644d9E25C3030863003;

    /// @notice WETH contract address
    address public constant WETH = 0x4200000000000000000000000000000000000006;

    /// @notice Slippage tolerance for buybacks (0.03%)
    uint24 private constant SLIPPAGE_TOLERANCE_BASIS_POINTS = 30;

    /// @notice Scaling factor for basis point calculations
    uint24 private constant SCALE = 1e6;

    /// @notice Address that receives burned WLD
    address public constant DEAD_ADDRESS = 0xDeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF;

    /// @notice Scaling factor for WLD price calculations
    uint256 public immutable WLD_USDC_SCALE;

    /// @notice Scaling factor for ETH price calculations
    uint256 public immutable ETH_USDC_SCALE;

    /// @notice ChainLink price feed for WLD/USD
    IChainLinkPriceFeed public WLD_USD_ORACLE = IChainLinkPriceFeed(0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0);

    /// @notice ChainLink price feed for ETH/USD
    IChainLinkPriceFeed public ETH_USD_ORACLE = IChainLinkPriceFeed(0xe1d72a719171DceAB9499757EB9d5AEb9e8D64A6);

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Timestamp of last buyback execution
    uint256 public lastBuybackTimestamp;

    /// @notice Minimum time between burn executions
    uint256 public minimumInterval;

    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when an address parameter is zero
    error InvalidAddress();

    /// @notice Thrown when slippage exceeds tolerance
    error InvalidSlippage();

    /// @notice Thrown when burn is attempted before minimum interval has passed
    error BurnTooSoon();

    /// @notice Thrown when contract has no ETH to burn
    error NothingToBurn();

    /// @notice Thrown when burn output is less than expected
    error InsufficientBurn(uint256 actualBurn, uint256 expectedBurn);

    /// @notice Thrown when oracle price data is expired or invalid
    error StalePrice();

    /// @notice Thrown when ETH transfer fails
    error ETHTransferFailed();

    /*/////////////////////////////////////////////////////////////////
                              EVENTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Emitted when a buyback is successfully executed
    event BuybackExecuted(uint256 indexed amountETH, uint256 indexed amountWLD, address indexed caller);

    constructor(address owner_, uint256 minimumInterval_) Ownable(owner_) {
        if (owner_ == address(0)) {
            revert InvalidAddress();
        }

        minimumInterval = minimumInterval_;

        WLD_USDC_SCALE = 10 ** uint128(WLD_USD_ORACLE.decimals());
        ETH_USDC_SCALE = 10 ** uint128(ETH_USD_ORACLE.decimals());

        lastBuybackTimestamp = block.timestamp;
    }

    /*//////////////////////////////////////////////////////////////
                            BURN EXECUTION
    //////////////////////////////////////////////////////////////*/

    /// @notice Execute a burn of all ETH held in the contract
    /// @dev Delegates execution and callback handling to RECIPIENT
    function executeBurn() external nonReentrant {
        if (block.timestamp <= lastBuybackTimestamp + minimumInterval) {
            revert BurnTooSoon();
        }

        uint128 currentPrice = price();
        uint256 balance = address(this).balance;

        uint256 expectedBurn = ((uint256(currentPrice) * balance) * (SCALE - SLIPPAGE_TOLERANCE_BASIS_POINTS) / SCALE);

        if (balance == 0) revert NothingToBurn();

        uint256 burnBalanceBefore = IERC20(WLD).balanceOf(DEAD_ADDRESS);

        IBurnCallback(msg.sender).burnCallback{value: balance}(expectedBurn, DEAD_ADDRESS);

        uint256 burnBalanceAfter = IERC20(WLD).balanceOf(DEAD_ADDRESS);

        uint256 totalBurned = burnBalanceAfter - burnBalanceBefore;

        if (totalBurned < expectedBurn) {
            revert InsufficientBurn(burnBalanceAfter - burnBalanceBefore, expectedBurn);
        }

        lastBuybackTimestamp = block.timestamp;

        emit BuybackExecuted(balance, burnBalanceAfter - burnBalanceBefore, msg.sender);
    }

    /// @notice Fetches and computes the current ETH/WLD price from ChainLink oracles
    /// @return currentPrice The current price as a 64.64 fixed point number
    function price() internal view returns (uint128 currentPrice) {
        IChainLinkPriceFeed.PriceFeedData memory wldUsdData = WLD_USD_ORACLE.priceFeed();
        IChainLinkPriceFeed.PriceFeedData memory ethUsdData = ETH_USD_ORACLE.priceFeed();

        if (
            wldUsdData.expiresAt < block.timestamp || ethUsdData.expiresAt < block.timestamp || wldUsdData.price <= 0
                || ethUsdData.price <= 0
        ) {
            revert StalePrice();
        }

        uint128 wldUsdPrice = uint128(int128(wldUsdData.price));
        uint128 ethUsdPrice = uint128(int128(ethUsdData.price));

        // ETH/WLD = (ETH/USD * WLD_SCALE) / (WLD/USD * ETH_SCALE)
        // Use 64-bit fixed point for precision
        // Copy immutables to stack for assembly access
        uint256 wldScale = WLD_USDC_SCALE;
        uint256 ethScale = ETH_USDC_SCALE;

        assembly ("memory-safe") {
            // (ethUsdPrice * wldScale * 2^64) / (wldUsdPrice * ethScale)
            let numerator := shl(64, mul(ethUsdPrice, wldScale))
            let denominator := mul(wldUsdPrice, ethScale)
            currentPrice := shr(64, div(numerator, denominator))
        }
    }

    ///@notice Helper function to transfer ETH.
    function _safeTransferETH(address to, uint256 amount) private {
        bool success;
        assembly ("memory-safe") {
            // Transfer the ETH and store if it succeeded or not.
            success := call(gas(), to, amount, 0, 0, 0, 0)
        }

        if (!success) {
            revert ETHTransferFailed();
        }
    }

    /*//////////////////////////////////////////////////////////////
                                 ADMIN
    //////////////////////////////////////////////////////////////*/

    /// @notice Sets the minimum interval between buybacks
    /// @param newInterval The new minimum interval in seconds
    function setInterval(uint256 newInterval) external onlyOwner {
        minimumInterval = newInterval;
    }

    /// @notice Withdraws ETH to a specified address
    /// @param to Recipient address
    /// @param amount Amount of ETH to withdraw
    function withdraw(address to, uint256 amount) external onlyOwner {
        _safeTransferETH(to, amount);
    }

    /// @notice Withdraws ERC20 tokens to a specified address
    /// @param token Token contract address
    /// @param to Recipient address
    /// @param amount Amount to withdraw
    function withdraw(address token, address to, uint256 amount) external onlyOwner {
        IERC20(token).transfer(to, amount);
    }

    /// @notice Sets the WLD/USD oracle address
    function setWLDUSDOracle(address oracle) external onlyOwner {
        WLD_USD_ORACLE = IChainLinkPriceFeed(oracle);
    }

    /// @notice Sets the ETH/USD oracle address
    function setETHUSDOracle(address oracle) external onlyOwner {
        ETH_USD_ORACLE = IChainLinkPriceFeed(oracle);
    }

    /// @notice Receive ETH sent to the contract
    receive() external payable {}
}
