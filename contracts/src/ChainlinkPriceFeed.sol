// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/**
 * @title PriceFeedStorage
 * @dev Contract for storing and updating price feed data for a single asset pair
 */
contract PriceFeedStorage {
    // Interface for the VerifierProxy contract
    interface IVerifierProxy {
        function verify(bytes memory payload, bytes memory parameterPayload) external returns (bytes);
    }

    // Struct to store price feed data
    struct PriceFeedData {
        int192 price;
        int192 bid;
        int192 ask;
        uint32 timestamp;
        uint32 expiresAt;
    }

    // Address of the pair token (always the first token in the pair)
    address public immutable pairAddress;

    // Address of the USDC token (always the second token in the pair)
    address public immutable usdcAddress;
    
    // Address of the LINK token
    address public immutable linkAddress;

    // Address of the VerifierProxy contract
    address public immutable verifierProxyAddress;

    // The unique identifier for this price feed
    bytes32 public immutable feedId;
    
    // Human-readable name of the pair (e.g., "ETH/USD")
    string public pairName;

    // Price feed data for the single pair
    PriceFeedData public priceFeed;

    // Events
    event PriceFeedUpdated(
        int192 price,
        int192 bid,
        int192 ask,
        uint32 timestamp,
        uint32 expiresAt
    );

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
        bytes32 _feedId,
        string memory _pairName
    ) {
        require(_usdcAddress != address(0), "Invalid USDC address");
        require(_pairAddress != address(0), "Invalid pair address");
        require(_verifierProxyAddress != address(0), "Invalid VerifierProxy address");
        require(_feedId != bytes32(0), "Invalid feed ID");
        require(_linkAddress != address(0), "Invalid LINK address");
        
        usdcAddress = _usdcAddress;
        pairAddress = _pairAddress;
        verifierProxyAddress = _verifierProxyAddress;
        feedId = _feedId;
        pairName = _pairName;
        linkAddress = _linkAddress;

        // TODO: Remove me, LINK payments are not required in production
        // Approve the maximum amount of LINK tokens for spending by this contract
        IERC20(linkAddress).approve(address(this), type(uint256).max);
    }


    function updatePriceData(bytes memory verifyReportRequest) public returns (bool success, bytes memory returnData) {
        // Perform a low-level call to the verifierProxy.verify function
        (success, returnData) = address(verifierProxy).call(
            abi.encodeWithSignature("verify(bytes,bytes)", verifyReportRequest, bytes(linkAddress))
        );

        // Check if the call was successful
        require(success, "Price data verification failed");

        // Decode the return data into the specified structure
        (
            bytes32 receivedFeedId,
            uint32 validFromTimestamp,
            uint32 observationsTimestamp,
            uint192 nativeFee,
            uint192 linkFee,
            uint32 expiresAt,
            int192 price,
            int192 bid,
            int192 ask
        ) = abi.decode(returnData, (bytes32, uint32, uint32, uint192, uint192, uint32, int192, int192, int192));

        // Verify that the feed ID matches the contract's feed ID
        require(receivedFeedId == feedId, "Feed ID does not match");

        // Validate the expiration times
        require(block.timestamp >= validFromTimestamp, "Price data is not yet valid");
        require(block.timestamp <= expiresAt, "Price data has expired");

        // Store the price feed data
        priceFeed = PriceFeedData({
            price: price,
            bid: bid,
            ask: ask,
            timestamp: observationsTimestamp,
            expiresAt: expiresAt
        });

        // Emit an event with the updated price feed data
        emit PriceFeedUpdated(price, bid, ask, observationsTimestamp, expiresAt);
    }

    /**
     * @dev Get the latest price feed data
     * @return price The latest price
     * @return bid The latest bid price
     * @return ask The latest ask price
     * @return timestamp The timestamp of the latest update
     * @return expiresAt The expiration timestamp
     */
    function getLatestPriceFeed() external view returns (
        int192 price,
        int192 bid,
        int192 ask,
        uint32 timestamp,
        uint32 expiresAt
    ) {
        require(priceFeed.timestamp > 0, "Price feed not available");
        require(block.timestamp <= priceFeed.expiresAt, "Price feed has expired");
        
        return (
            priceFeed.price,
            priceFeed.bid,
            priceFeed.ask,
            priceFeed.timestamp,
            priceFeed.expiresAt
        );
    }

    /**
     * @dev Check if the price feed is valid and not expired
     * @return valid True if the price feed is valid and not expired
     */
    function isPriceFeedValid() external view returns (bool) {
        return priceFeed.timestamp > 0 && block.timestamp <= priceFeed.expiresAt;
    }
}