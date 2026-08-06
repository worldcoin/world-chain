// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {LibProof} from "../lib/LibProof.sol";

interface IERC1271 {
    function isValidSignature(bytes32 hash, bytes calldata signature) external view returns (bytes4);
}

/// @title SecurityCouncilVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
contract SecurityCouncilVerifier is IWorldChainProofVerifier {
    /// @notice `bytes4(keccak256("isValidSignature(bytes32,bytes)"))`
    bytes4 internal constant ERC1271_MAGIC_VALUE = 0x1626ba7e;

    /// @notice Domain tag for council attestations over a proposal root.
    bytes32 public constant ATTESTATION_TYPEHASH = keccak256("WorldChainCouncilAttestation(bytes32 rootId)");

    /// @notice The council Safe.
    address public immutable council;

    /// @notice Thrown when constructor args are invalid.
    error InvalidAddress(address);

    constructor(address council_) {
        if (council_ == address(0)) revert InvalidAddress(council_);

        if (council_.code.length == 0) revert InvalidAddress(council_);
        council = council_;
    }

    /// @dev EIP-712 Attestation digest.
    function attestationDigest(bytes32 rootId) public view returns (bytes32) {
        return keccak256(abi.encode(ATTESTATION_TYPEHASH, block.chainid, address(this), rootId));
    }

    /// @inheritdoc IWorldChainProofVerifier
    /// @dev The council attests the proposal identity (`rootId`) rather than the transition.
    function verify(bytes32 rootId, LibProof.TransitionPublicValues calldata, bytes calldata proof)
        external
        view
        returns (bool)
    {
        try IERC1271(council).isValidSignature(attestationDigest(rootId), proof) returns (bytes4 magic) {
            return magic == ERC1271_MAGIC_VALUE;
        } catch {
            return false;
        }
    }
}
