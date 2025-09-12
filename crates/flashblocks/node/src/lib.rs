use clap::Args;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hex::FromHex;

pub fn parse_sk(s: &str) -> eyre::Result<SigningKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn parse_vk(s: &str) -> eyre::Result<VerifyingKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

#[derive(Args, Clone, Debug)]
#[group(requires = "flashblocks")]
pub struct FlashblocksNodeArgs {
    /// Enable Flashblocks client
    #[arg(long, env, required = false)]
    pub flashblocks: bool,

    #[arg(
        long = "flashblocks.authorizor_vk",
        env = "FLASHBLOCKS_AUTHORIZOR_VK",
        value_parser = parse_vk,
        required = false,
    )]
    pub authorizor_vk: VerifyingKey,
}
