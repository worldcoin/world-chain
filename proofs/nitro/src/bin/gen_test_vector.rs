//! One-shot binary to generate a deterministic P-384 test vector for p384_hints tests.
//! Run with: cargo run -p world-chain-proof-nitro --bin gen-test-vector
//! The output is the expected `collect_hints` byte string for the fixed vector.
use p384::ecdsa::{Signature, SigningKey, signature::Signer};
use sha2::{Digest, Sha384};
use world_chain_proof_nitro::p384_hints::collect_hints;

fn main() {
    // Fixed 48-byte private scalar (all 0x01 bytes — a known weak key, fine for test vectors)
    let sk_bytes = [1u8; 48];
    let sk = SigningKey::from_bytes((&sk_bytes).into()).expect("valid key");
    let vk = sk.verifying_key();
    let encoded = vk.to_encoded_point(false);
    let pubkey = &encoded.as_bytes()[1..]; // drop 0x04

    let message = b"p384_hints deterministic test vector v1";
    let sig: Signature = sk.sign(message);
    let sig_bytes = sig.to_bytes();

    let hash = Sha384::digest(message).to_vec();

    println!("// Private key (hex): {}", hex::encode(sk_bytes));
    println!("// Message: {:?}", std::str::from_utf8(message).unwrap());
    println!("// SHA-384(message): {}", hex::encode(hash));
    println!("// Signature (r||s): {}", hex::encode(sig_bytes));
    println!("// Public key (x||y): {}", hex::encode(pubkey));

    let hints = collect_hints(&hash, &sig_bytes, pubkey).expect("collect_hints");
    println!(
        "// Hints ({} bytes = {} inverses):",
        hints.len(),
        hints.len() / 48
    );
    println!("// {}", hex::encode(&hints));
}
