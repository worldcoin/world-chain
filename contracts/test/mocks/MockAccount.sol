// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IAccount} from "@account-abstraction/contracts/interfaces/IAccount.sol";
import {IAccountExecute} from "@account-abstraction/contracts/interfaces/IAccountExecute.sol";
import {IAggregator} from "@account-abstraction/contracts/interfaces/IAggregator.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";

contract MockAccount is IAccount, IAccountExecute {
    address public pbhAggregator;

    constructor(address _pbhAggregator) {
        pbhAggregator = _pbhAggregator;
    }

    function validateUserOp(PackedUserOperation calldata, bytes32, uint256)
        external
        view
        returns (uint256 validationData)
    {
        // Just return the pbhAggregator address as the authorizer
        // inclusion time = ∞
        validationData = uint256(uint160(pbhAggregator));
    }

    function executeUserOp(PackedUserOperation calldata userOp, bytes32 userOpHash) external {
        // Do nothing
    }
}
