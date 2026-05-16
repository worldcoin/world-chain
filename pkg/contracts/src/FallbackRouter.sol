// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";

import {IWorldChainAccountHooks} from "./interfaces/IWorldChainAccountHooks.sol";
import {IWorldChainAccountRouter} from "./interfaces/IWorldChainAccountRouter.sol";
import {IWorldChainSessionVerifier} from "./interfaces/IWorldChainSessionVerifier.sol";

/// @title FallbackRouter
/// @author 0xOsiris, World Contributors
/// @custom:security-contact security@toolsforhumanity.com
abstract contract FallbackRouter {
    /// @notice Thrown when a selector dispatched to the fallback is unknown.
    error UnknownSelector(bytes4 selector);

    /// @notice Thrown when a session verifier address is not in the installed key ring.
    error VerifierNotInstalled(address verifier);

    /// @notice Thrown when the caller is not authorized for the requested operation.
    error Unauthorized();

    /// @notice The installed admin verifier implementation address.
    address internal _adminVerifier;

    /// @notice Mapping of authorized session verifier address → installation payload.
    /// @dev Acts as the membership set for the current key ring. A verifier is installed iff its
    ///      mapping slot has non-empty installation bytes.
    mapping(address verifier => bytes installation) internal _sessionVerifierInstallation;

    fallback() external payable {
        bytes4 selector;
        assembly {
            // Calldata layout: [4-byte selector || abi-encoded args]. Read the top 4 bytes of the
            // first 32-byte calldata word.
            selector := shr(224, calldataload(0))
        }

        if (selector == IWorldChainAccountRouter.isValidSignatureForAdmin.selector) {
            handleAdminSignature();
        } else if (selector == IWorldChainAccountRouter.isValidSignatureForVerifier.selector) {
            handleSessionSignature();
        } else if (selector == IWorldChainAccountRouter.evaluateSessionPolicyForVerifier.selector) {
            handleSessionPolicy();
        } else if (selector == IWorldChainAccountRouter.installAdmin.selector) {
            handleInstallAdmin();
        } else if (selector == IWorldChainAccountRouter.installKeyRing.selector) {
            handleInstallKeyRing();
        } else {
            revert UnknownSelector(selector);
        }
    }

    function handleAdminSignature() internal {
        // TODO: FIXME
    }

    function handleSessionSignature() internal {
        // TODO: FIXME
    }

    function handleSessionPolicy() internal {
        // TODO: FIXME
    }

    function handleInstallAdmin() internal {
        // TODO: FIXME
    }

    function handleInstallKeyRing() internal {
        // TODO: FIXME
    }

    function forward(address target, bytes memory payload) internal {
        assembly {
            let success := delegatecall(gas(), target, add(payload, 0x20), mload(payload), 0, 0)
            let size := returndatasize()
            returndatacopy(0, 0, size)
            switch success
            case 0 { revert(0, size) }
            default { return(0, size) }
        }
    }
}


