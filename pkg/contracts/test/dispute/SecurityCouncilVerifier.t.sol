// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";

import {SecurityCouncilVerifier} from "../../src/dispute/council/SecurityCouncilVerifier.sol";
import {LibProof} from "../../src/dispute/lib/LibProof.sol";

import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {SafeProxyFactory} from "@safe-global/safe-contracts/contracts/proxies/SafeProxyFactory.sol";
import {
    CompatibilityFallbackHandler
} from "@safe-global/safe-contracts/contracts/handler/CompatibilityFallbackHandler.sol";
import {SignMessageLib} from "@safe-global/safe-contracts/contracts/libraries/SignMessageLib.sol";
import {Enum} from "@safe-global/safe-contracts/contracts/common/Enum.sol";

/// @dev Exercised against a real Safe 1.4.1 rather than a stub: the whole contract is a thin
///      delegation to Safe's EIP-1271 path, so a stub would only test the assertion, not the
///      integration that can actually break.
contract SecurityCouncilVerifierTest is Test {
    Safe internal safe;
    SecurityCouncilVerifier internal verifier;
    SignMessageLib internal signMessageLib;
    CompatibilityFallbackHandler internal handler;

    // Sorted ascending by address — Safe's checkSignatures requires increasing owner order.
    uint256 internal pk1;
    uint256 internal pk2;
    uint256 internal pk3;
    address internal owner1;
    address internal owner2;
    address internal owner3;

    bytes32 internal constant ROOT_ID = keccak256("rootId");

    function _transition() internal pure returns (LibProof.TransitionPublicValues memory) {
        return LibProof.TransitionPublicValues({
            l1Head: bytes32(0),
            l2PreRoot: bytes32(0),
            l2PreBlockNumber: 0,
            l2PostRoot: bytes32(0),
            l2PostBlockNumber: 0,
            rollupConfigHash: bytes32(0)
        });
    }
    uint256 internal constant THRESHOLD = 2;

    function setUp() public {
        (pk1, pk2, pk3) = (0xA11CE, 0xB0B, 0xC0FFEE);
        address[3] memory unsorted = [vm.addr(pk1), vm.addr(pk2), vm.addr(pk3)];
        uint256[3] memory keys = [pk1, pk2, pk3];
        // Insertion sort by address so signature blobs are always in Safe's required order.
        for (uint256 i = 1; i < 3; i++) {
            for (uint256 j = i; j > 0 && unsorted[j - 1] > unsorted[j]; j--) {
                (unsorted[j - 1], unsorted[j]) = (unsorted[j], unsorted[j - 1]);
                (keys[j - 1], keys[j]) = (keys[j], keys[j - 1]);
            }
        }
        (owner1, owner2, owner3) = (unsorted[0], unsorted[1], unsorted[2]);
        (pk1, pk2, pk3) = (keys[0], keys[1], keys[2]);

        handler = new CompatibilityFallbackHandler();
        signMessageLib = new SignMessageLib();
        Safe singleton = new Safe();
        SafeProxyFactory factory = new SafeProxyFactory();

        address[] memory owners = new address[](3);
        (owners[0], owners[1], owners[2]) = (owner1, owner2, owner3);

        safe = Safe(payable(address(factory.createProxyWithNonce(address(singleton), "", 0))));
        safe.setup(owners, THRESHOLD, address(0), "", address(handler), address(0), 0, payable(address(0)));

        verifier = new SecurityCouncilVerifier(address(safe));
    }

    /*//////////////////////////////////////////////////////////////
                              HELPERS
    //////////////////////////////////////////////////////////////*/

    /// @dev The hash Safe owners actually sign: the verifier digest, wrapped by the handler as
    ///      `abi.encode(digest)` and then in Safe's EIP-712 `SafeMessage`.
    function _safeMessageHash(bytes32 rootId) internal view returns (bytes32) {
        return handler.getMessageHashForSafe(safe, abi.encode(verifier.attestationDigest(rootId)));
    }

    function _sign(uint256 pk, bytes32 hash) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, hash);
        return abi.encodePacked(r, s, v);
    }

    /*//////////////////////////////////////////////////////////////
                    PATH 1 — AGGREGATED SIGNATURES
    //////////////////////////////////////////////////////////////*/

    function test_verify_AggregatedSignatures_AtThreshold() public view {
        bytes32 h = _safeMessageHash(ROOT_ID);
        bytes memory sigs = abi.encodePacked(_sign(pk1, h), _sign(pk2, h));
        assertTrue(verifier.verify(ROOT_ID, _transition(), sigs));
    }

    function test_verify_AggregatedSignatures_AboveThreshold() public view {
        bytes32 h = _safeMessageHash(ROOT_ID);
        bytes memory sigs = abi.encodePacked(_sign(pk1, h), _sign(pk2, h), _sign(pk3, h));
        assertTrue(verifier.verify(ROOT_ID, _transition(), sigs));
    }

    function test_verify_RevertsInsideSafe_AreCaughtAsFalse_BelowThreshold() public view {
        bytes memory sigs = _sign(pk1, _safeMessageHash(ROOT_ID));
        assertFalse(verifier.verify(ROOT_ID, _transition(), sigs));
    }

    function test_verify_ReturnsFalse_NonOwnerSignature() public view {
        bytes32 h = _safeMessageHash(ROOT_ID);
        uint256 intruderPk = 0xDEADBEEF;
        // Two signatures, but one is not an owner. Ordering still ascending is not guaranteed,
        // so either the owner check or the ordering check rejects it — both must be `false`.
        bytes memory sigs = abi.encodePacked(_sign(pk1, h), _sign(intruderPk, h));
        assertFalse(verifier.verify(ROOT_ID, _transition(), sigs));
    }

    /// @dev The core replay guard: signatures collected for one root must not satisfy another.
    function test_verify_ReturnsFalse_SignaturesForDifferentRootId() public view {
        bytes32 other = keccak256("some other root");
        bytes32 h = _safeMessageHash(other);
        bytes memory sigs = abi.encodePacked(_sign(pk1, h), _sign(pk2, h));
        assertFalse(verifier.verify(ROOT_ID, _transition(), sigs));
        assertTrue(verifier.verify(other, _transition(), sigs));
    }

    function test_verify_ReturnsFalse_GarbageProof() public view {
        assertFalse(verifier.verify(ROOT_ID, _transition(), hex"deadbeef"));
    }

    /*//////////////////////////////////////////////////////////////
              PATH 2 — PRE-APPROVED ON-CHAIN (approveHash)
    //////////////////////////////////////////////////////////////*/

    /// @dev `v=1` marks an approved-hash entry; `r` carries the owner address.
    function test_verify_PreApprovedHash_viaApproveHash() public {
        bytes32 h = _safeMessageHash(ROOT_ID);
        vm.prank(owner1);
        safe.approveHash(h);
        vm.prank(owner2);
        safe.approveHash(h);

        bytes memory sigs = abi.encodePacked(
            bytes32(uint256(uint160(owner1))),
            bytes32(0),
            uint8(1),
            bytes32(uint256(uint160(owner2))),
            bytes32(0),
            uint8(1)
        );
        assertTrue(verifier.verify(ROOT_ID, _transition(), sigs));
    }

    function test_verify_ReturnsFalse_ApprovedHashBelowThreshold() public {
        bytes32 h = _safeMessageHash(ROOT_ID);
        vm.prank(owner1);
        safe.approveHash(h);

        bytes memory sigs = abi.encodePacked(bytes32(uint256(uint160(owner1))), bytes32(0), uint8(1));
        assertFalse(verifier.verify(ROOT_ID, _transition(), sigs));
    }

    /*//////////////////////////////////////////////////////////////
          PATH 3 — PRE-APPROVED MESSAGE (signMessage, empty proof)
    //////////////////////////////////////////////////////////////*/

    function test_verify_PreApprovedMessage_EmptyProof() public {
        bytes memory message = abi.encode(verifier.attestationDigest(ROOT_ID));
        _execFromSafe(
            address(signMessageLib), abi.encodeCall(SignMessageLib.signMessage, (message)), Enum.Operation.DelegateCall
        );
        assertTrue(verifier.verify(ROOT_ID, _transition(), ""));
    }

    function test_verify_ReturnsFalse_EmptyProofWithoutApproval() public view {
        assertFalse(verifier.verify(ROOT_ID, _transition(), ""));
    }

    /// @dev signMessage only covers the root it was signed for.
    function test_verify_PreApprovedMessage_DoesNotCoverOtherRoots() public {
        bytes memory message = abi.encode(verifier.attestationDigest(ROOT_ID));
        _execFromSafe(
            address(signMessageLib), abi.encodeCall(SignMessageLib.signMessage, (message)), Enum.Operation.DelegateCall
        );
        assertFalse(verifier.verify(keccak256("unapproved root"), _transition(), ""));
    }

    /*//////////////////////////////////////////////////////////////
                        DIGEST + CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    function test_attestationDigest_BindsRootId(bytes32 a, bytes32 b) public view {
        vm.assume(a != b);
        assertTrue(verifier.attestationDigest(a) != verifier.attestationDigest(b));
    }

    function test_attestationDigest_BindsChainId() public {
        bytes32 before = verifier.attestationDigest(ROOT_ID);
        vm.chainId(block.chainid + 1);
        assertTrue(verifier.attestationDigest(ROOT_ID) != before);
    }

    function test_attestationDigest_BindsVerifierInstance() public {
        SecurityCouncilVerifier other = new SecurityCouncilVerifier(address(safe));
        assertTrue(other.attestationDigest(ROOT_ID) != verifier.attestationDigest(ROOT_ID));
    }

    function test_constructor_RevertIf_ZeroCouncil() public {
        vm.expectRevert(abi.encodeWithSelector(SecurityCouncilVerifier.InvalidAddress.selector, address(0)));
        new SecurityCouncilVerifier(address(0));
    }

    function test_constructor_RevertIf_CouncilIsEOA() public {
        address eoa = address(0xBEEF);
        vm.expectRevert(abi.encodeWithSelector(SecurityCouncilVerifier.InvalidAddress.selector, eoa));
        new SecurityCouncilVerifier(eoa);
    }

    /*//////////////////////////////////////////////////////////////
                          SAFE TX PLUMBING
    //////////////////////////////////////////////////////////////*/

    function _execFromSafe(address to, bytes memory data, Enum.Operation op) internal {
        bytes32 txHash =
            safe.getTransactionHash(to, 0, data, op, 0, 0, 0, address(0), payable(address(0)), safe.nonce());
        bytes memory sigs = abi.encodePacked(_sign(pk1, txHash), _sign(pk2, txHash));
        assertTrue(safe.execTransaction(to, 0, data, op, 0, 0, 0, address(0), payable(address(0)), sigs));
    }
}
