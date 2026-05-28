use ed25519_dalek::SigningKey;
use hex::FromHex;

fn main() {
    let sk_hex = std::env::args()
        .nth(1)
        .expect("Usage: sk-to-vk <secret_key_hex>");

    let bytes = <[u8; 32]>::from_hex(sk_hex.trim()).expect("invalid hex (expected 64 hex chars)");
    let sk = SigningKey::from_bytes(&bytes);
    let vk = sk.verifying_key();

    println!("{}", hex::encode(vk.as_bytes()));
}
