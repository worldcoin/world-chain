// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {IWldEthOracle} from "../interfaces/IWldEthOracle.sol";
import {IAggregatorV3} from "../interfaces/IAggregatorV3.sol";

/**
 * @title ChainlinkWldEthOracle
 * @notice {IWldEthOracle} implementation backed by two Chainlink feeds on World
 *         Chain: WLD/USD and ETH/USD. This is the pricing source for the
 *         paymaster, swappable behind {IWldEthOracle}.
 *
 * @dev Live World Chain feeds (both `ChainlinkPriceFeed`, 18 decimals):
 *        WLD/USD  0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0
 *        ETH/USD  0xe1d72a719171DceAB9499757EB9d5AEb9e8D64A6
 *
 *      Pricing is a simple cross: `WLD per ETH = ethUsd / wldUsd`. Both WLD and
 *      ETH have 18 token decimals, so no token-decimal adjustment is needed; only
 *      the feeds' own `decimals()` are normalised to 1e18 (read at construction,
 *      not assumed — these feeds report 18, unlike mainnet's 8).
 *
 *      Failure handling is fail-closed, per {IWldEthOracle}: a non-positive
 *      answer, an unset `updatedAt`, or an answer older than `maxStaleness`
 *      reverts. That propagates out of `validatePaymasterUserOp` and rejects the
 *      UserOperation rather than sponsoring gas at an unknown price. It also
 *      makes `triggerBatchSwap` refuse to swap without a trustworthy min-out
 *      bound. Stateless and immutable, so it is swappable via `setOracle(...)`.
 *
 *      Trade-off vs pool-derived pricing: removes in-protocol (pool) manipulation
 *      surface and decouples the price source from the swap venue, but adds a liveness
 *      dependency on the feed's push cadence — hence `maxStaleness`, which must
 *      be set comfortably above the feeds' heartbeat or ops will be rejected
 *      whenever the feed is merely quiet.
 */
contract ChainlinkWldEthOracle is IWldEthOracle {
    uint256 internal constant TARGET_DECIMALS = 18;

    /// @notice Chainlink WLD/USD feed.
    IAggregatorV3 public immutable wldUsdFeed;
    /// @notice Chainlink ETH/USD feed.
    IAggregatorV3 public immutable ethUsdFeed;
    /// @notice Multiplier normalising `wldUsdFeed` answers to 1e18.
    uint256 public immutable wldUsdScale;
    /// @notice Multiplier normalising `ethUsdFeed` answers to 1e18.
    uint256 public immutable ethUsdScale;
    /// @notice Max age (seconds) of a feed answer before it is treated as unusable.
    uint256 public immutable maxStaleness;

    error AmountTooLarge();
    error ZeroAddress();
    error InvalidStaleness();
    error UnsupportedDecimals(address feed, uint8 decimals);
    /// @dev Feed returned a non-positive price.
    error InvalidPrice(address feed, int256 answer);
    /// @dev Feed answer is older than `maxStaleness` (or `updatedAt` is unset).
    error StalePrice(address feed, uint256 updatedAt);

    /// @param _wldUsdFeed Chainlink WLD/USD aggregator.
    /// @param _ethUsdFeed Chainlink ETH/USD aggregator.
    /// @param _maxStaleness Max accepted answer age in seconds; set above the
    ///        feeds' heartbeat with headroom (e.g. 1 hour for a 1h heartbeat pair).
    constructor(IAggregatorV3 _wldUsdFeed, IAggregatorV3 _ethUsdFeed, uint256 _maxStaleness) {
        if (address(_wldUsdFeed) == address(0) || address(_ethUsdFeed) == address(0)) revert ZeroAddress();
        if (_maxStaleness == 0) revert InvalidStaleness();

        wldUsdFeed = _wldUsdFeed;
        ethUsdFeed = _ethUsdFeed;
        maxStaleness = _maxStaleness;
        wldUsdScale = _scaleOf(_wldUsdFeed);
        ethUsdScale = _scaleOf(_ethUsdFeed);
    }

    /// @inheritdoc IWldEthOracle
    function wldForEth(uint256 ethWei) external view override returns (uint256 wldAmount) {
        if (ethWei > type(uint128).max) revert AmountTooLarge();
        (uint256 wldUsd, uint256 ethUsd) = _prices();
        // WLD out = ETH in * (USD per ETH) / (USD per WLD); both prices are 1e18-scaled.
        wldAmount = (ethWei * ethUsd) / wldUsd;
    }

    /// @inheritdoc IWldEthOracle
    function ethForWld(uint256 wldAmount) external view override returns (uint256 ethWei) {
        if (wldAmount > type(uint128).max) revert AmountTooLarge();
        (uint256 wldUsd, uint256 ethUsd) = _prices();
        ethWei = (wldAmount * wldUsd) / ethUsd;
    }

    /// @notice Current 1e18-scaled feed prices. Reverts if either feed is unusable.
    /// @dev Exposed for off-chain sanity checks / monitoring.
    function prices() external view returns (uint256 wldUsd, uint256 ethUsd) {
        return _prices();
    }

    function _prices() internal view returns (uint256 wldUsd, uint256 ethUsd) {
        wldUsd = _readPrice(wldUsdFeed, wldUsdScale);
        ethUsd = _readPrice(ethUsdFeed, ethUsdScale);
    }

    /// @dev Reads a feed, validating sign and freshness, and scales it to 1e18.
    function _readPrice(IAggregatorV3 feed, uint256 scale) internal view returns (uint256) {
        (, int256 answer,, uint256 updatedAt,) = feed.latestRoundData();
        if (answer <= 0) revert InvalidPrice(address(feed), answer);
        // updatedAt == 0 means the round is unset/incomplete; a future timestamp is
        // treated as fresh (the subtraction would otherwise underflow-revert anyway).
        if (updatedAt == 0 || (block.timestamp > updatedAt && block.timestamp - updatedAt > maxStaleness)) {
            revert StalePrice(address(feed), updatedAt);
        }
        return uint256(answer) * scale;
    }

    function _scaleOf(IAggregatorV3 feed) internal view returns (uint256) {
        uint8 d = feed.decimals();
        if (d > TARGET_DECIMALS) revert UnsupportedDecimals(address(feed), d);
        return 10 ** (TARGET_DECIMALS - d);
    }
}
