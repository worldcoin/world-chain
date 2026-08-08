// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

/**
 * @title IAggregatorV3
 * @notice Chainlink `AggregatorV3Interface` subset used by {ChainlinkWldEthOracle}.
 *
 * @dev Transcribed from the verified ABI of the live World Chain WLD/USD feed
 *      `0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0` (contract `ChainlinkPriceFeed`,
 *      a Chainlink Data Streams verifier wrapper exposing the standard
 *      AggregatorV3 read surface). Note these World Chain feeds report **18
 *      decimals**, not the 8 decimals common on Ethereum mainnet — the oracle
 *      reads `decimals()` at construction instead of assuming.
 *
 *      Only the view functions are declared; the feed's `updatePriceData` push
 *      entrypoint is irrelevant to consumers.
 */
interface IAggregatorV3 {
    /// @notice Number of decimals in the value returned by `latestRoundData`.
    function decimals() external view returns (uint8);

    /// @notice Human-readable feed name, e.g. "WLD/USD".
    function description() external view returns (string memory);

    /// @notice Feed interface version.
    function version() external view returns (uint256);

    /// @notice Latest price round.
    /// @return roundId The round id the answer belongs to.
    /// @return answer The price, scaled by 10**decimals().
    /// @return startedAt Timestamp the round started.
    /// @return updatedAt Timestamp the answer was last updated (staleness check).
    /// @return answeredInRound The round the answer was computed in.
    function latestRoundData()
        external
        view
        returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);

    /// @notice Data for a historical round.
    function getRoundData(uint80 roundId)
        external
        view
        returns (uint80, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
}
