// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title CWIACloneReader
/// @notice Reads clone-with-immutable-args (CWIA) calldata appended by the OP
///         Stack `DisputeGameFactory` when it clones a game implementation.
///         Reimplements the Solady `Clone` reader idiom used by
///         `FaultDisputeGame` (op-contracts/v3.0.0-rc.2) directly with assembly,
///         avoiding any external Solady/CWIA dependency.
/// @dev The factory clones via `address(impl).clone(abi.encodePacked(...))`,
///      which appends the immutable-args payload to runtime calldata followed
///      by a 2-byte big-endian length suffix. Argument offsets below are
///      relative to the start of the immutable-args region.
abstract contract CWIACloneReader {
    /// @notice Reads an immutable arg of type `address` at `_argOffset`.
    function _getArgAddress(uint256 _argOffset) internal pure returns (address arg_) {
        uint256 offset = _getImmutableArgsOffset();
        assembly {
            arg_ := shr(0x60, calldataload(add(offset, _argOffset)))
        }
    }

    /// @notice Reads an immutable arg of type `uint256` at `_argOffset`.
    function _getArgUint256(uint256 _argOffset) internal pure returns (uint256 arg_) {
        uint256 offset = _getImmutableArgsOffset();
        assembly {
            arg_ := calldataload(add(offset, _argOffset))
        }
    }

    /// @notice Reads an immutable arg of type `bytes32` at `_argOffset`.
    function _getArgBytes32(uint256 _argOffset) internal pure returns (bytes32 arg_) {
        uint256 offset = _getImmutableArgsOffset();
        assembly {
            arg_ := calldataload(add(offset, _argOffset))
        }
    }

    /// @notice Reads `_length` bytes of immutable args starting at `_argOffset`.
    function _getArgBytes(uint256 _argOffset, uint256 _length) internal pure returns (bytes memory arg_) {
        uint256 offset = _getImmutableArgsOffset();
        assembly {
            // Grab the free memory pointer.
            arg_ := mload(0x40)
            // Store the length of the bytes array.
            mstore(arg_, _length)
            // Copy the calldata into the bytes array.
            calldatacopy(add(arg_, 0x20), add(offset, _argOffset), _length)
            // Update the free memory pointer, word-aligned.
            mstore(0x40, add(add(arg_, 0x20), and(add(_length, 0x1f), not(0x1f))))
        }
    }

    /// @notice Returns the offset of the packed immutable args in calldata.
    /// @dev The CWIA payload is appended to calldata followed by a 2-byte
    ///      big-endian suffix whose value is `argsLength + 2` (the legacy
    ///      Solady CWIA encoding op-contracts/v3.0.0-rc.2 uses); the args region
    ///      therefore starts at `calldatasize - suffix`.
    function _getImmutableArgsOffset() internal pure returns (uint256 offset_) {
        assembly {
            offset_ := sub(calldatasize(), shr(0xf0, calldataload(sub(calldatasize(), 2))))
        }
    }
}
