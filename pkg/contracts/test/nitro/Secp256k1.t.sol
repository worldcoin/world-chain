// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {Secp256k1} from "../../src/proofs/nitro/libraries/Secp256k1.sol";

/// @dev Wraps the library functions in external calls so vm.expectRevert can
///      observe the revert at a lower depth than the cheatcode frame.
contract Secp256k1Caller {
    function normalize(bytes memory key) external view returns (bytes memory) {
        return Secp256k1.normalizeToUncompressed(key);
    }

    function ethAddress(bytes memory key) external pure returns (address) {
        return Secp256k1.ethAddressFromUncompressed(key);
    }
}

contract Secp256k1Test is Test {
    Secp256k1Caller caller;

    function setUp() public {
        caller = new Secp256k1Caller();
    }

    function _split(bytes memory key65) internal pure returns (uint256 x, uint256 y) {
        assembly {
            x := mload(add(key65, 33))
            y := mload(add(key65, 65))
        }
    }

    function _uncompressed(uint256 x, uint256 y) internal pure returns (bytes memory out) {
        out = new bytes(65);
        out[0] = 0x04;
        assembly {
            mstore(add(out, 33), x)
            mstore(add(out, 65), y)
        }
    }

    function _compressed(uint256 x, uint256 y) internal pure returns (bytes memory out) {
        out = new bytes(33);
        out[0] = (y & 1 == 1) ? bytes1(0x03) : bytes1(0x02);
        assembly {
            mstore(add(out, 33), x)
        }
    }

    function test_NormalizeUncompressed_RoundTripsRealKeys() public {
        for (uint256 i = 1; i <= 4; i++) {
            Vm.Wallet memory w = vm.createWallet(i);
            bytes memory full = _uncompressed(w.publicKeyX, w.publicKeyY);
            bytes memory comp = _compressed(w.publicKeyX, w.publicKeyY);
            bytes memory norm = caller.normalize(comp);
            assertEq(norm, full, "compressed key did not decompress to canonical full key");
        }
    }

    function test_NormalizeUncompressed_PassThroughUncompressed() public {
        Vm.Wallet memory w = vm.createWallet("pass-through");
        bytes memory full = _uncompressed(w.publicKeyX, w.publicKeyY);
        bytes memory out = caller.normalize(full);
        assertEq(out, full);
    }

    function test_NormalizeUncompressed_RevertsOnBadLength() public {
        vm.expectRevert(Secp256k1.InvalidSec1Encoding.selector);
        caller.normalize(hex"01020304");
    }

    function test_NormalizeUncompressed_RevertsOnBadPrefix() public {
        bytes memory bad = new bytes(33);
        bad[0] = 0x05; // not 0x02/0x03
        vm.expectRevert(Secp256k1.InvalidSec1Encoding.selector);
        caller.normalize(bad);
    }

    function test_NormalizeUncompressed_RevertsOnNonCurvePoint() public {
        // Pick an x with no valid y on the curve. Search a small range until we
        // find one (most random x values are QRs; non-QRs exist with density 1/2
        // but small-integer x's tend to land on the curve).
        uint256 p = Secp256k1.P;
        for (uint256 x = 1; x < 100; x++) {
            uint256 rhs = addmod(mulmod(mulmod(x, x, p), x, p), 7, p);
            // Euler's criterion: rhs is a QR iff rhs^((p-1)/2) == 1 mod p. We
            // skip the (rare) cases where it's a QR.
            uint256 legendre = _modexp(rhs, (p - 1) / 2, p);
            if (legendre == p - 1) {
                bytes memory bad = new bytes(33);
                bad[0] = 0x02;
                assembly { mstore(add(bad, 33), x) }
                vm.expectRevert(Secp256k1.PointNotOnCurve.selector);
                caller.normalize(bad);
                return;
            }
        }
        revert("no non-QR found in search range");
    }

    function _modexp(uint256 base, uint256 exp_, uint256 mod_) internal view returns (uint256 r) {
        assembly {
            let m := mload(0x40)
            mstore(m, 0x20)
            mstore(add(m, 0x20), 0x20)
            mstore(add(m, 0x40), 0x20)
            mstore(add(m, 0x60), base)
            mstore(add(m, 0x80), exp_)
            mstore(add(m, 0xa0), mod_)
            if iszero(staticcall(gas(), 0x05, m, 0xc0, m, 0x20)) { revert(0, 0) }
            r := mload(m)
        }
    }

    function test_EthAddressFromUncompressed_MatchesVmAddress() public {
        Vm.Wallet memory w = vm.createWallet("eth-addr");
        bytes memory full = _uncompressed(w.publicKeyX, w.publicKeyY);
        assertEq(caller.ethAddress(full), w.addr);
    }
}
