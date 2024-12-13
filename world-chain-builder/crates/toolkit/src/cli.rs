use bytes::Bytes;
use chrono::NaiveDate;
use clap::Parser;
use identity_source::IdentitySource;
use inclusion_proof_source::InclusionProofSource;
use world_chain_builder_pbh::external_nullifier::Prefix;

pub mod identity_source;
pub mod inclusion_proof_source;
mod utils;

/// A CLI utility for proving raw Ethereum transactions
#[derive(Debug, Clone, Parser)]
#[clap(version, about)]
pub struct Opt {
    #[clap(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Clone, Parser)]
pub enum Cmd {
    /// Proves a transaction and returns a hex encoded payload ready to be sent to a World Chain Builder
    ///
    /// Note that it's necessary to provide the identity and inclusion proof
    /// and there exist multiple ways to provide them
    ///
    /// For the identity in testing the simplest way is to use a predefined identity secret via `-I 11ff11` flag or `export IDENTITY=11ff11` env var
    ///
    /// For the inclusion proof you can fetch it dynamically from the (staging) sequencer API via `--inclusion-proof-url https://signup-orb-ethereum.stage-crypto.worldcoin.dev/inclusionProof`
    /// or `export INCLUSION_PROOF_URL=https://signup-orb-ethereum.stage-crypto.worldcoin.dev/inclusionProof` env var
    Prove(ProveArgs),
}

#[derive(Debug, Clone, Parser)]
pub struct ProveArgs {
    /// Raw tx
    ///
    /// can be constructed with `cast mktx`
    #[clap(short, long)]
    #[clap(value_parser = utils::bytes_parse_hex)]
    pub tx: Bytes,

    /// The PBH prefix
    #[clap(long, default_value = "v1")]
    pub prefix: Prefix,

    /// The PBH nonce for the priority tx
    ///
    /// should be in range 0-30 otherwise the tx will be discarded as invalid
    #[clap(short = 'N', long)]
    #[clap(alias = "nonce")]
    pub pbh_nonce: u16,

    /// Overrides the current date for PBH proof generation
    /// Format: "YYYY-MM-DD"
    ///
    /// Dates are always assumed to be in UTC
    #[clap(short = 'D', long)]
    pub custom_date: Option<NaiveDate>,

    #[command(flatten)]
    pub identity_source: IdentitySource,

    #[command(flatten)]
    pub inclusion_proof_source: InclusionProofSource,
}

#[derive(Debug, Clone, Parser)]
pub struct SendArgs {}
