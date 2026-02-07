// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IChainLinkPriceFeed} from "../../src/fees/FeeEscrow.sol";

/// @title MockChainLinkPriceFeed
/// @notice Mock price feed for devnet testing
contract MockChainLinkPriceFeed is IChainLinkPriceFeed {
    int192 public mockPrice;
    uint8 public mockDecimals;
    uint32 public priceValidityDuration;

    constructor(int192 _price, uint8 _decimals, uint32 _validityDuration) {
        mockPrice = _price;
        mockDecimals = _decimals;
        priceValidityDuration = _validityDuration;
    }

    function setPrice(int192 _price) external {
        mockPrice = _price;
    }

    function setDecimals(uint8 _decimals) external {
        mockDecimals = _decimals;
    }

    function setValidityDuration(uint32 _duration) external {
        priceValidityDuration = _duration;
    }

    function priceFeed() external view override returns (PriceFeedData memory) {
        return PriceFeedData({
            price: mockPrice,
            timestamp: uint32(block.timestamp),
            expiresAt: uint32(block.timestamp + priceValidityDuration)
        });
    }

    function latestRoundData()
        external
        view
        override
        returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound)
    {
        return (1, int256(mockPrice), block.timestamp, block.timestamp, 1);
    }

    function decimals() external view override returns (uint8) {
        return mockDecimals;
    }
}
