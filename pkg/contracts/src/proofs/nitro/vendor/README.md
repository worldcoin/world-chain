# Vendored dependencies

These directories contain third-party Solidity sources used by the on-chain AWS
Nitro Enclaves attestation stack. They are vendored (rather than added as Forge
submodules) so that the contracts package stays self-contained and the
remappings table stays minimal.

## `nitro-validator/`

Subset of [`base/nitro-validator`](https://github.com/base/nitro-validator) at
commit [`75c1145`][src]. The library performs:

- CBOR decoding of NSM/COSE_Sign1 attestation documents (`CborDecode.sol`);
- ASN.1 / X.509 certificate parsing (`Asn1Decode.sol`);
- AWS Nitro PKI chain validation with a cache to amortize cost
  (`CertManager.sol`, `ICertManager.sol`);
- P-384 ECDSA signature verification (`ECDSA384Curve.sol`, via the Solarity
  library below);
- Top-level `validateAttestation(attestationTbs, signature)` orchestration
  (`NitroValidator.sol`).

Only the import paths were modified — the `@solarity/...` imports were rewritten
to relative paths into `../solarity/`. Solidity pragma is `^0.8.15`, compatible
with this repo's `0.8.28` compiler.

[src]: https://github.com/base/nitro-validator/tree/75c11451d3d39f81bcc8dbc04ad706ed81e0b260

## `solarity/`

`ECDSA384.sol` and `MemoryUtils.sol` from
[`dl-solarity/solidity-lib`](https://github.com/dl-solarity/solidity-lib) at
commit `b9475719`. Only used transitively by `nitro-validator/CertManager.sol`
and `nitro-validator/ECDSA384Curve.sol` for ~8M-gas P-384 curve operations
(Strauss-Shamir double-scalar multiplication).
