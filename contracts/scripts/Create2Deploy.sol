// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title Create2Deploy
/// @notice A contract to deploy a contract using the CREATE2 opcode
contract Create2Factory {
    function deploy(bytes32 salt, bytes memory initCode) public returns (address addr) {
        assembly {
            addr := create2(0, add(initCode, 0x20), mload(initCode), salt)
            if iszero(extcodesize(addr)) {
                mstore(0x00, 0x2f8f8019)
                revert(0x1c, 0x04)
            }
        }
    }
}
