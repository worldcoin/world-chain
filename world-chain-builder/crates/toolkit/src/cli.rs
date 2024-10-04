use bytes::{Bytes, BytesMut};
use chrono::NaiveDate;
use clap::Parser;
use identity_source::IdentitySource;
use inclusion_proof_source::InclusionProofSource;

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
    /// Raw tx
    ///
    /// can be constructed with `cast mktx`
    #[clap(short, long)]
    #[clap(value_parser = utils::bytes_parse_hex)]
    pub tx: Bytes,

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
