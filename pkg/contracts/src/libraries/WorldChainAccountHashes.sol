// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldChainAccountManager} from "../interfaces/IWorldChainAccountManager.sol";
import {WorldChainAccountConstants} from "./WorldChainAccountConstants.sol";

/// @title WorldChainAccountHashes
/// @author Worldcoin
/// @notice Pure helpers for every WIP-1001 hash. All functions match the spec encoding
///         byte-for-byte and are domain-separated so a signature is only valid for one
///         (operation, chain, manager, account, nonce, key-ring) tuple.
/// @custom:security-contact security@toolsforhumanity.com
library WorldChainAccountHashes {
    /// @notice `adminHash = keccak256(abi.encode(admin))`.
    function adminHash(IWorldChainAccountManager.WorldChainAccountVerifier memory admin)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(admin));
    }

    /// @notice `keyRingHash = keccak256(abi.encode(sessionVerifiers))`.
    function keyRingHash(IWorldChainAccountManager.WorldChainAccountVerifier[] memory sessionVerifiers)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(sessionVerifiers));
    }

    /// @notice Derives the canonical WIP-1001 account address.
    /// @dev `account = address(uint160(uint256(keccak256(abi.encode(domain, chainId, manager,`
    ///      `routerCodeHash, adminHash, accountSalt)))))`.
    function deriveAccount(
        uint256 chainId,
        address manager,
        bytes32 routerCodeHash,
        bytes32 adminHash_,
        bytes32 accountSalt
    ) internal pure returns (address) {
        return address(
            uint160(
                uint256(
                    keccak256(
                        abi.encode(
                            WorldChainAccountConstants.WORLD_CHAIN_ACCOUNT_DOMAIN,
                            chainId,
                            manager,
                            routerCodeHash,
                            adminHash_,
                            accountSalt
                        )
                    )
                )
            )
        );
    }

    /// @notice The admin operation hash signed by the admin verifier at `create`.
    function createHash(
        uint256 chainId,
        address manager,
        bytes32 routerCodeHash,
        address account,
        bytes32 adminHash_,
        bytes32 accountSalt,
        bytes32 keyRingHash_
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                WorldChainAccountConstants.WORLD_CHAIN_ACCOUNT_CREATE_DOMAIN,
                chainId,
                manager,
                routerCodeHash,
                account,
                adminHash_,
                accountSalt,
                keyRingHash_
            )
        );
    }

    /// @notice The admin operation hash signed by the admin verifier at `setKeyRing`.
    function setKeyRingHash(
        uint256 chainId,
        address manager,
        address account,
        uint64 adminNonce,
        bytes32 expectedCurrentKeyRingHash,
        bytes32 newKeyRingHash
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                WorldChainAccountConstants.WORLD_CHAIN_ACCOUNT_SET_DOMAIN,
                chainId,
                manager,
                account,
                adminNonce,
                expectedCurrentKeyRingHash,
                newKeyRingHash
            )
        );
    }
}
