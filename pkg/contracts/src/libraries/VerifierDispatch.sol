// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {WorldChainAccountConstants} from "./WorldChainAccountConstants.sol";

/// @title VerifierDispatch
/// @author Worldcoin
/// @notice Low-level helpers for dispatching to verifier implementations with strict gas,
///         calldata, and return-data discipline.
/// @dev The manager uses {staticcallEip1271} to invoke the router; the router uses
///      {delegatecallEip1271} / {delegatecallPolicy} to invoke verifier implementations under
///      its own storage. Helpers reject malformed returndata (which the protocol treats as a
///      verification failure) and never propagate revert bubbles.
///
///      Although `DELEGATECALL` may semantically mutate state, the manager always wraps these
///      dispatches in an outer `STATICCALL` from the manager to the router so the verifier's
///      execution is non-state-mutating end-to-end. To express that view-ness in the type
///      system we expose the delegatecall helpers as `internal view` via a function-pointer
///      cast (the same pattern used in Solady's `LibCall`).
/// @custom:security-contact security@toolsforhumanity.com
library VerifierDispatch {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                ENCODING                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice ABI-encodes `IERC1271.isValidSignature(hash, signature)` into a freshly
    ///         allocated `bytes`.
    function encodeIsValidSignature(bytes32 hash, bytes memory signature) internal pure returns (bytes memory) {
        // 0x1626ba7e — IERC1271.isValidSignature.selector.
        return abi.encodeWithSelector(0x1626ba7e, hash, signature);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              STATIC DISPATCH                            ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Performs a `STATICCALL` to `target` with exactly `gasLimit` gas and accepts the
    ///         call only when it returns a 32-byte word whose high 4 bytes equal the EIP-1271
    ///         magic value.
    /// @dev Returns `false` on revert, out-of-gas, malformed returndata, or any non-magic
    ///      return. Never reverts.
    function staticcallEip1271(address target, uint64 gasLimit, bytes memory callData) internal view returns (bool ok) {
        bytes4 magic = WorldChainAccountConstants.EIP1271_MAGIC_VALUE;
        assembly ("memory-safe") {
            let success := staticcall(gasLimit, target, add(callData, 0x20), mload(callData), 0x00, 0x20)
            ok := and(
                and(success, eq(returndatasize(), 0x20)),
                eq(and(mload(0x00), 0xffffffff00000000000000000000000000000000000000000000000000000000), magic)
            )
        }
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            DELEGATE DISPATCH                            ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Performs a `DELEGATECALL` to `target` that is expected to return an EIP-1271
    ///         magic value. Returns the bytes4 result, or {EIP1271_FAILURE_VALUE} on revert /
    ///         malformed return.
    /// @dev Declared `view` via a function-pointer cast. Callers MUST invoke this only inside
    ///      an outer `STATICCALL` frame (the manager guarantees this for all on-protocol
    ///      validation paths). Calling it from a non-static frame will allow the delegate to
    ///      mutate caller state — the helper itself imposes no such restriction.
    function delegatecallEip1271(address target, uint64 gasLimit, bytes memory callData)
        internal
        view
        returns (bytes4 result)
    {
        function(address, uint64, bytes memory) internal returns (bytes4) impl = _delegatecallEip1271;
        function(address, uint64, bytes memory) internal view returns (bytes4) asView;
        assembly ("memory-safe") {
            asView := impl
        }
        return asView(target, gasLimit, callData);
    }

    /// @notice Performs a `DELEGATECALL` to `target` that is expected to return a bool.
    /// @dev See {delegatecallEip1271} for the view-ness caveats.
    function delegatecallPolicy(address target, uint64 gasLimit, bytes memory callData)
        internal
        view
        returns (bool allowed)
    {
        function(address, uint64, bytes memory) internal returns (bool) impl = _delegatecallPolicy;
        function(address, uint64, bytes memory) internal view returns (bool) asView;
        assembly ("memory-safe") {
            asView := impl
        }
        return asView(target, gasLimit, callData);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                          PRIVATE IMPLEMENTATIONS                        ///
    ///////////////////////////////////////////////////////////////////////////////

    function _delegatecallEip1271(address target, uint64 gasLimit, bytes memory callData)
        private
        returns (bytes4 result)
    {
        bytes4 failure = WorldChainAccountConstants.EIP1271_FAILURE_VALUE;
        assembly ("memory-safe") {
            let success := delegatecall(gasLimit, target, add(callData, 0x20), mload(callData), 0x00, 0x20)
            switch and(success, eq(returndatasize(), 0x20))
            case 1 { result := and(mload(0x00), 0xffffffff00000000000000000000000000000000000000000000000000000000) }
            default { result := failure }
        }
    }

    function _delegatecallPolicy(address target, uint64 gasLimit, bytes memory callData)
        private
        returns (bool allowed)
    {
        assembly ("memory-safe") {
            let success := delegatecall(gasLimit, target, add(callData, 0x20), mload(callData), 0x00, 0x20)
            allowed := and(and(success, eq(returndatasize(), 0x20)), eq(mload(0x00), 1))
        }
    }
}
