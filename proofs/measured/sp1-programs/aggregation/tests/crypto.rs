use alloy_primitives::{Signature, U256, address, b256, keccak256};
use sha2::{Digest, Sha256};

#[test]
fn hash_known_vectors() {
    assert_eq!(
        Sha256::digest(b"abc").as_slice(),
        b256!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").as_slice()
    );
    assert_eq!(
        Sha256::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq").as_slice(),
        b256!("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1").as_slice()
    );
    assert_eq!(
        keccak256(b"abc"),
        b256!("4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45")
    );
}

#[test]
fn recover_sender_with_high_s() {
    // Alloy's non-normalized recovery vector exercises Ethereum's high-S handling.
    // https://github.com/alloy-rs/core/blob/v1.6.0/crates/primitives/src/signature/sig.rs
    let signature: Signature = "48b55bfa915ac795c431978d8a6a992b628d557da5ff759b307d495a36649353efffd310ac743f371de3b9f7f9cb56c0b28ad43601b4ab949f53faa07bd2c8041b"
        .parse()
        .unwrap();
    let digest = b256!("5eb4f5a33c621f32a8622d5f943b6b102994dfe4e5aebbefe69bb1b2aa0fc93e");
    assert_eq!(
        signature.recover_address_from_prehash(&digest).unwrap(),
        address!("0f65fe9276bc9a24ae7083ae28e2660ef72df99e")
    );
    assert!(
        Signature::new(U256::ZERO, U256::ZERO, false)
            .recover_address_from_prehash(&digest)
            .is_err()
    );
}
