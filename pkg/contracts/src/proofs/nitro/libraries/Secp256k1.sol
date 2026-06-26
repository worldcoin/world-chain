// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title Secp256k1
/// @author Worldcoin
/// @notice secp256k1 SEC1 public-key utilities used by the Nitro stack.
/// @dev secp256k1 curve: y² = x³ + 7 mod p, with
///        p = 2^256 − 2^32 − 977
///          = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
///      Since p ≡ 3 (mod 4), the square root of `rhs` mod p (when one exists)
///      is `rhs^((p+1)/4) mod p`, computed via the modexp precompile (0x05).
library Secp256k1 {
    /// @notice secp256k1 field modulus p.
    uint256 internal constant P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F;

    /// @notice (p + 1) / 4, used to take square roots in F_p.
    uint256 internal constant SQRT_EXP = 0x3FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFBFFFFF0C;

    /// @notice Thrown when the input has an unexpected length or prefix.
    error InvalidSec1Encoding();

    /// @notice Thrown when the supplied x-coordinate has no valid y on the curve
    ///         (i.e. `x³ + 7` is a quadratic non-residue mod p).
    error PointNotOnCurve();

    /// @notice Returns the canonical 65-byte SEC1-uncompressed (`0x04 || X || Y`)
    ///         encoding of `key`, accepting either:
    ///           - a 33-byte SEC1-compressed input (`0x02`/`0x03` || X), or
    ///           - a 65-byte SEC1-uncompressed input.
    /// @dev    Reverts on any other length, prefix, or non-curve point.
    function normalizeToUncompressed(bytes memory key) internal view returns (bytes memory out) {
        uint256 len = key.length;
        if (len == 65) {
            if (uint8(key[0]) != 0x04) revert InvalidSec1Encoding();
            return key;
        }
        if (len != 33) revert InvalidSec1Encoding();

        uint8 prefix = uint8(key[0]);
        if (prefix != 0x02 && prefix != 0x03) revert InvalidSec1Encoding();

        uint256 x;
        // key layout: 32-byte length, 1-byte prefix, 32-byte X. X starts at offset 33.
        assembly {
            x := mload(add(key, 33))
        }
        if (x >= P) revert InvalidSec1Encoding();

        // rhs = (x³ + 7) mod p
        uint256 rhs = addmod(mulmod(mulmod(x, x, P), x, P), 7, P);

        // y = rhs^((p+1)/4) mod p
        uint256 y = _modexp(rhs, SQRT_EXP, P);

        // Confirm we got an actual square root; otherwise the point is not on the curve.
        if (mulmod(y, y, P) != rhs) revert PointNotOnCurve();

        // Pick the sign that matches the prefix (`0x03` ⇒ odd y, `0x02` ⇒ even y).
        bool wantOdd = prefix == 0x03;
        if (((y & 1) == 1) != wantOdd) y = P - y;

        out = new bytes(65);
        assembly {
            // out[0] = 0x04
            mstore8(add(out, 32), 0x04)
            mstore(add(out, 33), x)
            mstore(add(out, 65), y)
        }
    }

    /// @notice Returns the 20-byte Ethereum address derived from a 65-byte
    ///         SEC1-uncompressed secp256k1 key (`0x04 || X || Y`): the last 20
    ///         bytes of `keccak256(X || Y)`.
    function ethAddressFromUncompressed(bytes memory uncompressed) internal pure returns (address) {
        if (uncompressed.length != 65 || uint8(uncompressed[0]) != 0x04) {
            revert InvalidSec1Encoding();
        }
        bytes32 h;
        assembly {
            // Skip the 32-byte length and the 0x04 prefix; hash the 64-byte X||Y.
            h := keccak256(add(uncompressed, 33), 64)
        }
        return address(uint160(uint256(h)));
    }

    /// @dev Computes `base^exponent mod modulus` via the EVM modexp precompile.
    function _modexp(uint256 base, uint256 exponent, uint256 modulus) private view returns (uint256 result) {
        assembly {
            let m := mload(0x40)
            mstore(m, 0x20) // baseLen
            mstore(add(m, 0x20), 0x20) // expLen
            mstore(add(m, 0x40), 0x20) // modLen
            mstore(add(m, 0x60), base)
            mstore(add(m, 0x80), exponent)
            mstore(add(m, 0xa0), modulus)
            if iszero(staticcall(gas(), 0x05, m, 0xc0, m, 0x20)) { revert(0, 0) }
            result := mload(m)
        }
    }
}
