use std::path::PathBuf;

use alloy_consensus::{TxEnvelope, TypedTransaction};
use alloy_rlp::Decodable;
use bytes::{Bytes, BytesMut};
use clap::{Args, Parser};
use semaphore::hash_to_field;
use semaphore::identity::Identity;

#[derive(Debug, Clone, Parser)]
struct Opt {
    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Clone, Parser)]
enum Cmd {
    Prove(ProveArgs),

    Send(SendArgs),
}

#[derive(Debug, Clone, Parser)]
struct ProveArgs {
    #[clap(short, long)]
    #[clap(value_parser = parse_hex)]
    tx: Bytes,

    #[clap(short, long)]
    #[clap(alias = "nonce")]
    pbh_nonce: usize,

    #[command(flatten)]
    identity_source: IdentitySource,
}

fn parse_hex(s: &str) -> eyre::Result<BytesMut> {
    Ok(BytesMut::from(&hex::decode(s.trim_start_matches("0x"))?[..]))
}

#[derive(Debug, Clone, Args)]
struct IdentitySource {
    #[clap(
        short = 'I',
        long,
        conflicts_with = "identity_file",
        required_unless_present = "identity_file",
        value_parser = parse_hex
    )]
    pub identity: Option<BytesMut>,

    #[clap(
        long,
        conflicts_with = "identity",
        required_unless_present = "identity"
    )]
    pub identity_file: Option<PathBuf>,
}

impl IdentitySource {
    fn load(&self) -> Identity {
        if let Some(mut identity) = self.identity.clone() {
            return Identity::from_secret(identity.as_mut(), None);
        }

        if let Some(identity_file) = &self.identity_file {
            let mut identity = std::fs::read(identity_file).unwrap();
            return Identity::from_secret(identity.as_mut(), None);
        }

        unreachable!()
    }
}

#[derive(Debug, Clone, Parser)]
struct SendArgs {}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenvy::dotenv().ok();

    let args = Opt::parse();
    println!("{:?}", args);

    match args.cmd {
        Cmd::Prove(prove_args) => {
            let tx: TxEnvelope = TxEnvelope::decode(&mut prove_args.tx.as_ref())?;

            let tx_hash = tx.tx_hash();
            let signal_hash = hash_to_field(tx_hash.as_ref());

            let identity = prove_args.identity_source.load();

            semaphore::protocol::generate_proof(
                &identity,
                merkle_proof,
                external_nullifier_hash,
                signal_hash,
            );
        }
        _ => unimplemented!(),
    }

    Ok(())
}
