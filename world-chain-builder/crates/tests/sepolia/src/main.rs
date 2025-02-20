use clap::Parser;
use cli::identities::generate_identities;
use cli::transactions::{create_bundle, send_bundle};
use cli::Cli;
use reqwest::header::HeaderValue;
use reth_rpc_layer::{Claims, JwtSecret};

mod cli;

#[derive(Parser, Debug, Clone)]
pub struct Args {
    /// The number of PBH transactions to send to WC Sepolia Testnet
    #[clap(long, short, default_value_t = 1)]
    pub pbh_batch_size: u8,
    /// The number of Non-PBH transactions to send to WC Sepolia Testnet
    #[clap(long, short, default_value_t = 0)]
    pub tx_batch_size: u8,
    /// Identity commitment of the PBH transactions
    #[clap(long, short)]
    pub identity_secret: String,
    /// The private key signer for the transactions
    #[clap(long)]
    pub private_key: String,
    /// The nonce for the wallet
    #[clap(long, short, default_value_t = 0)]
    pub nonce: u64,
    /// The RPC URL for WC Sepolia Testnet
    #[clap(long, short, required = true)]
    pub rpc_url: String,
}

pub const STAGE_SEQUENCER_ORB: &str = "https://signup-orb-ethereum.stage-crypto.worldcoin.dev";

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.command {
        cli::Commands::Generate(args) => generate_identities(args).await?,
        cli::Commands::Bundle(args) => create_bundle(args).await?,
        cli::Commands::Send(args) => send_bundle(args).await?,
    }
    Ok(())
}

/// Helper function to convert a secret into a Bearer auth header value with claims according to
/// <https://github.com/ethereum/execution-apis/blob/main/src/engine/authentication.md#jwt-claims>.
/// The token is valid for 60 seconds.
pub fn secret_to_bearer_header(secret: &JwtSecret) -> HeaderValue {
    let claim = Claims::with_current_timestamp();
    format!(
        "Bearer {}",
        secret
            .encode(&Claims {
                iat: claim.iat,
                exp: None,
            })
            .unwrap()
    )
    .parse()
    .unwrap()
}
