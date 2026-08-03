// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {MockNitroAttestationVerifier} from "./mocks/MockNitroAttestationVerifier.sol";

contract NitroEnclaveKeyRegistryTest is Test {
    MockNitroAttestationVerifier attestationVerifier;
    NitroEnclaveKeyRegistry registry;

    address owner = makeAddr("owner");
    address attacker = makeAddr("attacker");

    bytes32 constant PCR0 = bytes32(uint256(0xa));
    bytes32 constant PCR1 = bytes32(uint256(0xb));
    bytes32 constant PCR2 = bytes32(uint256(0xc));

    bytes constant TBS = hex"01010101";
    bytes constant SIG = hex"02020202";

    bytes pubKey;
    bytes otherKey;
    address signer;
    address otherSigner;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);

        pubKey = _key(0x01);
        otherKey = _key(0xAB);
        signer = _signer(pubKey);
        otherSigner = _signer(otherKey);
        attestationVerifier.setExpectation(TBS, SIG, pubKey, PCR0, PCR1, PCR2);
    }

    function _key(uint8 seed) internal pure returns (bytes memory key) {
        key = new bytes(65);
        key[0] = 0x04;
        for (uint8 i = 1; i < 65; i++) {
            key[i] = bytes1(seed + i);
        }
    }

    function _signer(bytes memory key) internal pure returns (address) {
        return address(uint160(uint256(keccak256(_slice64(key)))));
    }

    function _slice64(bytes memory key) internal pure returns (bytes memory coordinates) {
        coordinates = new bytes(64);
        for (uint256 i; i < 64; i++) {
            coordinates[i] = key[i + 1];
        }
    }

    function test_RegisterKey_StoresSignerAndEmits() public {
        vm.expectEmit(true, false, false, true);
        emit NitroEnclaveKeyRegistry.SignerRegistered(signer, PCR0, PCR1, PCR2);
        registry.registerKey(TBS, SIG, "");

        assertTrue(registry.isSignerRegistered(signer));
    }

    function test_RegisterKey_RevertsWhenVerifierRejects() public {
        // No expectation set for these (TBS, SIG).
        vm.expectRevert(MockNitroAttestationVerifier.UnexpectedCall.selector);
        registry.registerKey(hex"deadbeef", SIG, "");
    }

    function test_RegisterKey_RevertsOnMalformedKey() public {
        bytes memory badKey = hex"0301";
        attestationVerifier.setExpectation(hex"badf00", SIG, badKey, PCR0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"badf00", SIG, "");
    }

    function test_IsSignerRegistered_FalseForUnknown() public view {
        assertFalse(registry.isSignerRegistered(otherSigner));
    }

    function test_RevokeSigner_OnlyOwner() public {
        registry.registerKey(TBS, SIG, "");

        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, attacker));
        registry.revokeSigner(signer);

        vm.prank(owner);
        vm.expectEmit(true, false, false, true);
        emit NitroEnclaveKeyRegistry.SignerRevoked(signer);
        registry.revokeSigner(signer);

        assertFalse(registry.isSignerRegistered(signer));
    }

    function test_RevokeSigner_RevertsForUnregistered() public {
        vm.prank(owner);
        vm.expectRevert(NitroEnclaveKeyRegistry.SignerNotRegistered.selector);
        registry.revokeSigner(signer);
    }

    function test_MultipleInstancesSameImage() public {
        // Two validator instances run the same enclave image → same PCR triple,
        // but each has its own ephemeral key. Both must register successfully
        // and both must be queryable via isSignerRegistered. (No on-chain
        // PCRs → signer index exists; the SignerRegistered events are the off-chain
        // source of truth.)
        registry.registerKey(TBS, SIG, "");

        bytes memory tbs2 = hex"cafe";
        attestationVerifier.setExpectation(tbs2, SIG, otherKey, PCR0, PCR1, PCR2);
        registry.registerKey(tbs2, SIG, "");

        assertTrue(registry.isSignerRegistered(signer));
        assertTrue(registry.isSignerRegistered(otherSigner));
    }

    function test_Constructor_SetsOwnerAndVerifier() public view {
        assertEq(registry.owner(), owner);
        assertEq(address(registry.verifier()), address(attestationVerifier));
    }

    function test_Constructor_RevertsOnZeroOwner() public {
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableInvalidOwner.selector, address(0)));
        new NitroEnclaveKeyRegistry(attestationVerifier, address(0));
    }

    function test_RegisterKey_ReturnsSignerAndPCRs() public {
        // PCRs are forwarded from the verifier while the public key is reduced
        // to the same signer identity that ecrecover returns for proofs.
        (address registeredSigner, bytes32 p0, bytes32 p1, bytes32 p2) = registry.registerKey(TBS, SIG, "");
        assertEq(registeredSigner, signer);
        assertEq(p0, PCR0);
        assertEq(p1, PCR1);
        assertEq(p2, PCR2);
    }

    function test_RegisterKey_DerivesEthereumAddressFromAttestedKey() public {
        Vm.Wallet memory wallet = vm.createWallet("attested-enclave");
        bytes memory walletKey = abi.encodePacked(bytes1(0x04), wallet.publicKeyX, wallet.publicKeyY);
        bytes memory walletTbs = hex"0a11ce";
        attestationVerifier.setExpectation(walletTbs, SIG, walletKey, PCR0, PCR1, PCR2);

        (address registeredSigner,,,) = registry.registerKey(walletTbs, SIG, "");

        assertEq(registeredSigner, wallet.addr);
        assertTrue(registry.isSignerRegistered(wallet.addr));
    }

    function test_RegisterKey_RejectsValidLengthKeyWithWrongPrefix() public {
        // 65-byte key but the SEC1 prefix is 0x03 (compressed-y-odd) instead
        // of 0x04 (uncompressed). The registry's defensive check must catch
        // this even though the real verifier already enforces 0x04.
        bytes memory badKey = new bytes(65);
        badKey[0] = 0x03;
        for (uint8 i = 1; i < 65; i++) {
            badKey[i] = bytes1(i);
        }
        attestationVerifier.setExpectation(hex"feedface", SIG, badKey, PCR0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"feedface", SIG, "");
    }

    function test_RegisterKey_RejectsKeyWithLength64() public {
        // SEC1-uncompressed minus the 0x04 prefix — wrong length, must revert.
        bytes memory key64 = new bytes(64);
        for (uint256 i = 0; i < 64; i++) {
            key64[i] = bytes1(uint8(0xAA));
        }
        attestationVerifier.setExpectation(hex"6464", SIG, key64, PCR0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"6464", SIG, "");
    }

    function test_RegisterKey_RejectsKeyWithLength66() public {
        bytes memory key66 = new bytes(66);
        key66[0] = 0x04;
        attestationVerifier.setExpectation(hex"6666", SIG, key66, PCR0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"6666", SIG, "");
    }

    function test_RevokeSigner_AlreadyRevokedRevertsSignerNotRegistered() public {
        registry.registerKey(TBS, SIG, "");
        vm.prank(owner);
        registry.revokeSigner(signer);
        // A second revoke for the same (now Revoked) key must surface
        // `SignerNotRegistered`, NOT silently succeed and re-emit the event.
        vm.prank(owner);
        vm.expectRevert(NitroEnclaveKeyRegistry.SignerNotRegistered.selector);
        registry.revokeSigner(signer);
    }

    function test_RevokeSigner_RevertsForUnknownSigner() public {
        // Revoking a never-seen signer must surface `SignerNotRegistered`.
        vm.prank(owner);
        vm.expectRevert(NitroEnclaveKeyRegistry.SignerNotRegistered.selector);
        registry.revokeSigner(otherSigner);
    }

    function test_RegisterKey_EmitsSignerRegisteredWithExactPCRs() public {
        // Stronger assertion: emits the exact event including all PCRs.
        vm.expectEmit(true, false, false, true);
        emit NitroEnclaveKeyRegistry.SignerRegistered(signer, PCR0, PCR1, PCR2);
        registry.registerKey(TBS, SIG, "");
    }

    function test_SignerStatus_ForUnknownSignerIsZero() public view {
        // The default `SignerStatus.Unknown == 0` invariant: any never-seen signer
        // must report Unknown, isSignerRegistered=false, isSignerRevoked=false.
        assertEq(uint8(registry.signerStatus(otherSigner)), uint8(NitroEnclaveKeyRegistry.SignerStatus.Unknown));
        assertFalse(registry.isSignerRegistered(otherSigner));
        assertFalse(registry.isSignerRevoked(otherSigner));
    }

    function test_RegisterKey_PropagatesVerifierRevertVerbatim() public {
        // The verifier rejects; the registry must not swallow the revert.
        // (Already covered by `test_RegisterKey_RevertsWhenVerifierRejects`,
        // but here we lock in the exact selector so a future refactor that
        // wraps the call in a try/catch can't accidentally turn it into
        // a different error type.)
        vm.expectRevert(MockNitroAttestationVerifier.UnexpectedCall.selector);
        registry.registerKey(hex"deadbeef", hex"00", "");
    }

    function test_RevokeSigner_PreventsReregistration() public {
        registry.registerKey(TBS, SIG, "");

        vm.prank(owner);
        registry.revokeSigner(signer);
        assertFalse(registry.isSignerRegistered(signer));
        assertTrue(registry.isSignerRevoked(signer));

        // Anyone re-submitting the same attestation must fail.
        vm.expectRevert(NitroEnclaveKeyRegistry.SignerRevokedPermanently.selector);
        registry.registerKey(TBS, SIG, "");

        assertFalse(registry.isSignerRegistered(signer));
    }

    function test_RevokeSigner_AlsoBlocksRegistrationUnderDifferentPCRs() public {
        registry.registerKey(TBS, SIG, "");

        vm.prank(owner);
        registry.revokeSigner(signer);

        // Even if a doc later asserted the same key under different PCRs, the
        // revoke must be sticky on the derived signer.
        bytes32 otherPcr0 = bytes32(uint256(0xff));
        attestationVerifier.setExpectation(hex"1234", SIG, pubKey, otherPcr0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.SignerRevokedPermanently.selector);
        registry.registerKey(hex"1234", SIG, "");
    }

    function test_IsSignerRevoked_FalseBeforeRevoke() public {
        registry.registerKey(TBS, SIG, "");
        assertFalse(registry.isSignerRevoked(signer));
    }

    function test_RegisterKey_RevertsIfAlreadyActive() public {
        registry.registerKey(TBS, SIG, "");
        vm.expectRevert(NitroEnclaveKeyRegistry.SignerAlreadyRegistered.selector);
        registry.registerKey(TBS, SIG, "");
    }

    function test_SignerStatus_LifecycleTransitions() public {
        // Unknown
        assertEq(uint8(registry.signerStatus(signer)), uint8(NitroEnclaveKeyRegistry.SignerStatus.Unknown));

        // Active
        registry.registerKey(TBS, SIG, "");
        assertEq(uint8(registry.signerStatus(signer)), uint8(NitroEnclaveKeyRegistry.SignerStatus.Active));

        // Revoked
        vm.prank(owner);
        registry.revokeSigner(signer);
        assertEq(uint8(registry.signerStatus(signer)), uint8(NitroEnclaveKeyRegistry.SignerStatus.Revoked));
    }

    function test_MultiImageCoexistence() public {
        // Image A.
        registry.registerKey(TBS, SIG, "");

        // Image B: different PCR triple, different key, different attestation.
        bytes32 pcr0B = bytes32(uint256(0x10));
        bytes32 pcr1B = bytes32(uint256(0x11));
        bytes32 pcr2B = bytes32(uint256(0x12));
        bytes memory tbsB = hex"03030303";
        attestationVerifier.setExpectation(tbsB, SIG, otherKey, pcr0B, pcr1B, pcr2B);
        registry.registerKey(tbsB, SIG, "");

        // Both keys are registered.
        assertTrue(registry.isSignerRegistered(signer));
        assertTrue(registry.isSignerRegistered(otherSigner));
    }
}
