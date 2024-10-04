use bytes::{Bytes, BytesMut};
use clap::Parser;
use identity_source::IdentitySource;

pub mod identity_source;
pub mod inclusion_proof_source;
mod utils;

#[derive(Debug, Clone, Parser)]
pub struct Opt {
    #[clap(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Clone, Parser)]
pub enum Cmd {
    Prove(ProveArgs),

    Send(SendArgs),
}

#[derive(Debug, Clone, Parser)]
pub struct ProveArgs {
    #[clap(short, long)]
    #[clap(value_parser = parse_hex)]
    pub tx: Bytes,

    #[clap(short, long)]
    #[clap(alias = "nonce")]
    pub pbh_nonce: usize,

    #[command(flatten)]
    pub identity_source: IdentitySource,
}

#[derive(Debug, Clone, Parser)]
pub struct SendArgs {}
