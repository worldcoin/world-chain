// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {ISignatureValidator} from "@safe-global/safe-contracts/contracts/interfaces/ISignatureValidator.sol";

contract MockEIP1271SignatureValidator is ISignatureValidator {
    function isValidSignature(bytes memory, bytes memory) public pure override returns (bytes4) {
        return EIP1271_MAGIC_VALUE;
    }
}
