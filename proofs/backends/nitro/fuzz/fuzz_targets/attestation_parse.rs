#![no_main]

//! `parse_attestation_doc` walks hand-rolled CBOR over bytes an attacker controls, so the
//! contract is: never panic, never hang — return `Err` instead.

use libfuzzer_sys::fuzz_target;
use world_chain_proof_nitro_enclave::attestation::parse_attestation_doc;

fuzz_target!(|data: &[u8]| {
    // Malformed input must be rejected, not crash the host.
    if let Ok(doc) = parse_attestation_doc(data) {
        // Touch the decoded fields so a successful parse can't hide a bad length or index.
        let _ = doc.pcrs.len();
        let _ = doc.cabundle.iter().map(Vec::len).sum::<usize>();
        let _ = doc.certificate.as_ref().map(Vec::len);
        let _ = doc.public_key.as_ref().map(Vec::len);
        let _ = doc.nonce.as_ref().map(Vec::len);
        let _ = doc.module_id.as_deref();
    }
});
