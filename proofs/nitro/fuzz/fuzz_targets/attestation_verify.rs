#![no_main]

//! Same contract as `attestation_parse`, but one layer deeper: this reaches the COSE_Sign1
//! decode, the DER certificate parsing and the P-384 signature path.

use libfuzzer_sys::fuzz_target;
use world_chain_proof_nitro::attestation::verify_cose_sign1_signature;

fuzz_target!(|data: &[u8]| {
    // A fuzzer will not forge a valid P-384 signature; the point is that failing to verify
    // is an Err rather than a panic, an index-out-of-bounds or an unbounded allocation.
    let _ = verify_cose_sign1_signature(data);
});
