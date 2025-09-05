// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "@openzeppelin/contracts/access/Ownable.sol";

/**
 * @title FlashbotsAuthorizerRegistry
 * @dev All flashbots enabled nodes will use this contract to get the authorizer address
 * and expect a valid signature from the authorize
 */
contract FlashbotsAuthorizerRegistry is Ownable {
    address public flashbotsAuthorizer;

    event FlashbotsAuthorizerUpdated(address indexed previousAuthorizer, address indexed newAuthorizer);

    error ZeroAddress();

    /**
     * @dev Constructor sets the initial owner and flashbots authorizer
     * @param initialAuthorizer The initial flashbots authorizer address
     */
    constructor(address initialAuthorizer) Ownable(msg.sender) {
        require(initialAuthorizer != address(0), ZeroAddress());
        flashbotsAuthorizer = initialAuthorizer;
        emit FlashbotsAuthorizerUpdated(address(0), initialAuthorizer);
    }

    /**
     * @dev Updates the flashbots authorizer address
     * Can only be called by the contract owner
     * @param newAuthorizer The new flashbots authorizer address
     */
    function updateFlashbotsAuthorizer(address newAuthorizer) external onlyOwner {
        require(newAuthorizer != address(0), ZeroAddress());

        address previousAuthorizer = flashbotsAuthorizer;
        flashbotsAuthorizer = newAuthorizer;

        emit FlashbotsAuthorizerUpdated(previousAuthorizer, newAuthorizer);
    }

    /**
     * @dev Returns the current flashbots authorizer address
     * @return The current flashbots authorizer address
     */
    function getFlashbotsAuthorizer() external view returns (address) {
        return flashbotsAuthorizer;
    }
}
