// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {TransitionPublicValues} from "../lib/LibProof.sol";

/// @title IWorldChainProofVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainProofVerifier {
    /// @notice Verifies that `proof` attests `transition` for the proposal identified by
    ///         `rootId`. The calling game supplies both, so implementations enforce the
    ///         expected transition by construction: the ZK and TEE lanes bind `proof` to
    ///         `transition`, the council lane attests `rootId` directly.
    function verify(bytes32 rootId, TransitionPublicValues calldata transition, bytes calldata proof)
        external
        view
        returns (bool);
}
