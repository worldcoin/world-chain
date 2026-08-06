# Fuzz targets

`parse_attestation_doc` and `verify_cose_sign1_signature` decode hand-rolled CBOR, DER and
P-384 signatures from bytes an attacker controls. The contract both targets check is that
malformed input returns `Err` rather than panicking, over-allocating or hanging.

This crate is not at the repository root, so `cargo fuzz` needs `--fuzz-dir` unless you cd in
first — otherwise it looks for `./fuzz/Cargo.toml` and reports a missing manifest.

```bash
# from the repository root
cargo +nightly fuzz run --fuzz-dir proofs/backends/nitro/fuzz attestation_parse  -- -max_total_time=60
cargo +nightly fuzz run --fuzz-dir proofs/backends/nitro/fuzz attestation_verify -- -max_total_time=60

# or
cd proofs/backends/nitro/fuzz && cargo +nightly fuzz run attestation_parse -- -max_total_time=60
```

Seed the corpus from a known-good document before a long run:

```bash
mkdir -p corpus/attestation_parse && cp seeds/* corpus/attestation_parse/
```

`corpus/`, `artifacts/` and `coverage/` are gitignored per `cargo fuzz init`'s default
template; only the seed inputs in `seeds/` are tracked.

`Cargo.lock` is committed and copied from the parent workspace. Without it this crate
re-resolves `kona`/`alloy` and fails to build.
