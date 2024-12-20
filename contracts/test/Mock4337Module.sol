// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {PBHSafe4337Module} from "../src/PBH4337Module.sol";
import {Safe4337Module} from "@4337/Safe4337Module.sol";
import {PackedUserOperation} from "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {ValidationData} from "@account-abstraction/contracts/core/Helpers.sol";
import {_packValidationData} from "@account-abstraction/contracts/core/Helpers.sol";
import {ISafe} from "@4337/interfaces/Safe.sol";
import "forge-std/console.sol";

contract Mock4337Module is PBHSafe4337Module {
    constructor(address entryPoint, address _pbhSignatureAggregator, uint192 _pbhNonceKey)
        PBHSafe4337Module(entryPoint, _pbhSignatureAggregator, _pbhNonceKey)
    {}

    function validateSignaturesExternal(PackedUserOperation calldata userOp)
        external
        view
        returns (uint256 validationData)
    {
        console.log("validateSignaturesExternal");
        return _validateSignatures(userOp);
    }
}
