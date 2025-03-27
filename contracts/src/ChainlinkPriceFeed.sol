// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

interface IVerifierProxy {
    function verify(bytes memory payload, bytes memory parameterPayload) external returns (bytes memory);
}

/**
 * @title PriceFeedStorage
 * @dev Contract for storing and updating price feed data for a single asset pair
 */
contract ChainlinkPriceFeed {
    // Custom errors
    error InvalidAddress(string parameterName);
    error InvalidFeedId();
    error VerificationFailed();
    error FeedIdMismatch();
    error PriceDataNotYetValid();
    error PriceDataExpired();
    error PriceFeedNotAvailable();
    error PriceFeedExpired();

    // Struct to store price feed data
    struct PriceFeedData {
        int192 price;
        uint32 timestamp;
        uint32 expiresAt;
    }

    // Address of the pair token (always the first token in the pair)
    address public immutable PAIR_TOKEN_ADDRESS;

    // Address of the USDC token (always the second token in the pair)
    address public immutable USDC_TOKEN_ADDRESS;

    // Address of the LINK token
    address public immutable LINK_TOKEN_ADDRESS;

    // Address of the VerifierProxy contract
    address public immutable VERIFIER_PROXY_ADDRESS;

    // The unique identifier for this price feed
    bytes32 public immutable FEED_ID;

    // Human-readable name of the pair (e.g., "WLD/USD")
    string public PAIR_NAME;

    // Price feed data for the single pair
    PriceFeedData public priceFeed;

    event PriceFeedUpdated(int192 price, uint32 timestamp, uint32 expiresAt);

    /**
     * @dev Constructor to set the USDC address, Pair Token address, VerifierProxy address, feed ID and pair name
     * @param _pairAddress The address of the pair token token
     * @param _usdcAddress The address of the USDC token
     * @param _linkAddress The address of the LINK token
     * @param _verifierProxyAddress The address of the VerifierProxy contract
     * @param _feedId The unique identifier for this price feed
     * @param _pairName Human-readable name of the pair (e.g., "ETH/USD")
     */
    constructor(
        address _pairAddress,
        address _usdcAddress,
        address _linkAddress,
        address _verifierProxyAddress,
        // TODO: REMOVE NOT NEEDED AFTER LINK REQUIREMENT IS REMOVED
        address _rewardManagerAddress,
        bytes32 _feedId,
        string memory _pairName
    ) {
        if (_usdcAddress == address(0)) revert InvalidAddress("USDC");
        if (_pairAddress == address(0)) revert InvalidAddress("Pair");
        if (_verifierProxyAddress == address(0)) revert InvalidAddress("VerifierProxy");
        if (_feedId == bytes32(0)) revert InvalidFeedId();
        if (_linkAddress == address(0)) revert InvalidAddress("LINK");

        USDC_TOKEN_ADDRESS = _usdcAddress;
        PAIR_TOKEN_ADDRESS = _pairAddress;
        VERIFIER_PROXY_ADDRESS = _verifierProxyAddress;
        FEED_ID = _feedId;
        PAIR_NAME = _pairName;
        LINK_TOKEN_ADDRESS = _linkAddress;

        // TODO: REMOVE NOT NEEDED AFTER LINK REQUIREMENT IS REMOVED
        IERC20(LINK_TOKEN_ADDRESS).approve(address(_rewardManagerAddress), type(uint256).max);
    }

    function updatePriceData(bytes memory verifyReportRequest, bytes memory parameterPayload)
        public
        returns (bytes memory)
    {
        bytes memory returnDataCall =
            IVerifierProxy(VERIFIER_PROXY_ADDRESS).verify(verifyReportRequest, parameterPayload);

        // Decode the return data into the specified structure
        (
            bytes32 receivedFeedId,
            uint32 validFromTimestamp,
            uint32 observationsTimestamp,
            ,
            ,
            uint32 expiresAt,
            int192 price,
            ,
        ) = abi.decode(returnDataCall, (bytes32, uint32, uint32, uint192, uint192, uint32, int192, int192, int192));

        // Verify that the feed ID matches the contract's feed ID
        if (receivedFeedId != FEED_ID) revert FeedIdMismatch();

        // Validate the expiration times
        if (block.timestamp < validFromTimestamp) revert PriceDataNotYetValid();
        if (block.timestamp > expiresAt) revert PriceDataExpired();

        // Store the price feed data
        priceFeed = PriceFeedData({price: price, timestamp: observationsTimestamp, expiresAt: expiresAt});

        // Emit an event with the updated price feed data
        emit PriceFeedUpdated(price, observationsTimestamp, expiresAt);

        return returnDataCall;
    }

    /**
     * @dev Get the latest price feed data
     * @return price The latest price
     * @return timestamp The timestamp of the latest update
     * @return expiresAt The expiration timestamp
     */
    function getLatestPriceFeed() external view returns (int192 price, uint32 timestamp, uint32 expiresAt) {
        if (priceFeed.timestamp == 0) revert PriceFeedNotAvailable();
        if (block.timestamp > priceFeed.expiresAt) revert PriceFeedExpired();

        return (priceFeed.price, priceFeed.timestamp, priceFeed.expiresAt);
    }

    /**
     * @dev Check if the price feed is valid and not expired
     * @return valid True if the price feed is valid and not expired
     */
    function isPriceFeedValid() external view returns (bool) {
        return priceFeed.timestamp > 0 && block.timestamp <= priceFeed.expiresAt;
    }
}
