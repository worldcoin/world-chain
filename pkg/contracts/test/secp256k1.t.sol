// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {SekP256K1Signer} from "../src/signers/secp256k1.sol";

contract SekP256K1SignerTest is Test {
    /// @dev Mirrors `SIGNER_SLOT` in the contract under test.
    bytes32 internal constant SIGNER_SLOT = keccak256("worldchain.admin.verifier.secp256k1.v1.signer");

    SekP256K1Signer internal verifier;

    address internal signerAddr;
    uint256 internal signerPk;

    function setUp() public {
        verifier = new SekP256K1Signer();
        (signerAddr, signerPk) = makeAddrAndKey("signer");
    }

    /* ---------------------------------- install --------------------------------- */

    function test_Install_WritesSignerToNamespacedSlot() public {
        verifier.install(abi.encode(signerAddr));

        bytes32 stored = vm.load(address(verifier), SIGNER_SLOT);
        assertEq(address(uint160(uint256(stored))), signerAddr);
    }

    function test_Install_RevertsWhenZeroSigner() public {
        vm.expectRevert(SekP256K1Signer.ZeroSigner.selector);
        verifier.install(abi.encode(address(0)));
    }

    function test_Install_RevertsWhenAlreadyInstalled() public {
        verifier.install(abi.encode(signerAddr));

        vm.expectRevert(SekP256K1Signer.AlreadyInstalled.selector);
        verifier.install(abi.encode(makeAddr("other")));
    }

    function test_Install_RevertsWhenAlreadyInstalledWithSameSigner() public {
        verifier.install(abi.encode(signerAddr));

        vm.expectRevert(SekP256K1Signer.AlreadyInstalled.selector);
        verifier.install(abi.encode(signerAddr));
    }

    function test_Install_RevertsOnEmptyPayload() public {
        vm.expectRevert(SekP256K1Signer.InvalidInstallation.selector);
        verifier.install("");
    }

    function testFuzz_Install_RevertsOnWrongLengthPayload(bytes calldata payload) public {
        vm.assume(payload.length != 32);
        vm.expectRevert(SekP256K1Signer.InvalidInstallation.selector);
        verifier.install(payload);
    }

    function testFuzz_Install_AcceptsAnyNonZeroSigner(address addr) public {
        vm.assume(addr != address(0));

        verifier.install(abi.encode(addr));

        bytes32 stored = vm.load(address(verifier), SIGNER_SLOT);
        assertEq(address(uint160(uint256(stored))), addr);
    }

    /// @dev `abi.decode(payload, (address))` masks to the low 160 bits, so any 32-byte payload
    ///      whose low 160 bits are non-zero is a valid installation.
    function testFuzz_Install_IgnoresUpperBitsOfAddressWord(uint256 raw) public {
        address expected = address(uint160(raw));
        vm.assume(expected != address(0));

        verifier.install(abi.encode(raw));

        bytes32 stored = vm.load(address(verifier), SIGNER_SLOT);
        assertEq(address(uint160(uint256(stored))), expected);
    }

    /* ------------------------------ isValidSignature ----------------------------- */

    function test_IsValidSignature_ReturnsMagicValueOnValidSignature() public {
        verifier.install(abi.encode(signerAddr));

        bytes32 hash = keccak256("eip-1271 digest");
        bytes memory sig = _sign(signerPk, hash);

        assertEq(verifier.isValidSignature(hash, sig), IERC1271.isValidSignature.selector);
    }

    function test_IsValidSignature_AcceptsEip2098CompactSignature() public {
        verifier.install(abi.encode(signerAddr));

        bytes32 hash = keccak256("compact");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerPk, hash);
        // EIP-2098: pack v's parity into the top bit of s.
        bytes32 vs = bytes32(uint256(s) | (uint256(v - 27) << 255));
        bytes memory sig = abi.encodePacked(r, vs);

        assertEq(verifier.isValidSignature(hash, sig), IERC1271.isValidSignature.selector);
    }

    function test_IsValidSignature_RevertsWhenNoSignerInstalled() public {
        bytes32 hash = keccak256("hello");
        bytes memory sig = _sign(signerPk, hash);

        vm.expectRevert(abi.encodeWithSelector(SekP256K1Signer.InvalidSignature.selector, "No signer installed"));
        verifier.isValidSignature(hash, sig);
    }

    function test_IsValidSignature_RevertsOnBadSignatureLength() public {
        verifier.install(abi.encode(signerAddr));

        bytes32 hash = keccak256("hello");
        bytes memory sig = hex"deadbeef"; // 4 bytes, neither 64 nor 65

        vm.expectRevert(abi.encodeWithSelector(SekP256K1Signer.InvalidSignature.selector, "ECDSA recovery failed"));
        verifier.isValidSignature(hash, sig);
    }

    function test_IsValidSignature_RevertsOnHighSValue() public {
        verifier.install(abi.encode(signerAddr));

        bytes32 hash = keccak256("malleable");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerPk, hash);

        // Flip into the upper half of the curve to trigger ECDSA.RecoverError.InvalidSignatureS.
        uint256 n = SECP256K1_ORDER;
        bytes32 highS = bytes32(n - uint256(s));
        uint8 flippedV = v == 27 ? 28 : 27;
        bytes memory sig = abi.encodePacked(r, highS, flippedV);

        vm.expectRevert(abi.encodeWithSelector(SekP256K1Signer.InvalidSignature.selector, "ECDSA recovery failed"));
        verifier.isValidSignature(hash, sig);
    }

    function test_IsValidSignature_RevertsOnSignerMismatch() public {
        verifier.install(abi.encode(signerAddr));

        (, uint256 otherPk) = makeAddrAndKey("intruder");
        bytes32 hash = keccak256("wrong-signer");
        bytes memory sig = _sign(otherPk, hash);

        vm.expectRevert(
            abi.encodeWithSelector(
                SekP256K1Signer.InvalidSignature.selector, "Signature signer does not match installed signer"
            )
        );
        verifier.isValidSignature(hash, sig);
    }

    function test_IsValidSignature_RevertsOnTamperedHash() public {
        verifier.install(abi.encode(signerAddr));

        bytes32 hash = keccak256("original");
        bytes memory sig = _sign(signerPk, hash);

        vm.expectRevert(
            abi.encodeWithSelector(
                SekP256K1Signer.InvalidSignature.selector, "Signature signer does not match installed signer"
            )
        );
        verifier.isValidSignature(keccak256("tampered"), sig);
    }

    function testFuzz_IsValidSignature_AcceptsAnyValidPair(bytes32 hash, uint256 pk) public {
        pk = bound(pk, 1, SECP256K1_ORDER - 1);
        address addr = vm.addr(pk);

        verifier.install(abi.encode(addr));

        bytes memory sig = _sign(pk, hash);
        assertEq(verifier.isValidSignature(hash, sig), IERC1271.isValidSignature.selector);
    }

    function testFuzz_IsValidSignature_RejectsForeignSigner(bytes32 hash, uint256 installedPk, uint256 attackerPk)
        public
    {
        installedPk = bound(installedPk, 1, SECP256K1_ORDER - 1);
        attackerPk = bound(attackerPk, 1, SECP256K1_ORDER - 1);
        vm.assume(vm.addr(installedPk) != vm.addr(attackerPk));

        verifier.install(abi.encode(vm.addr(installedPk)));

        bytes memory sig = _sign(attackerPk, hash);
        vm.expectRevert(
            abi.encodeWithSelector(
                SekP256K1Signer.InvalidSignature.selector, "Signature signer does not match installed signer"
            )
        );
        verifier.isValidSignature(hash, sig);
    }

    /* ----------------------------------- helpers --------------------------------- */

    function _sign(uint256 pk, bytes32 hash) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, hash);
        return abi.encodePacked(r, s, v);
    }
}
