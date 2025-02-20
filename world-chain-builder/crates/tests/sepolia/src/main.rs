use std::str::FromStr;
use alloy_provider::network::EthereumWallet;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_signer_local::PrivateKeySigner;
use alloy_transport_http::Http;
use base64::prelude::BASE64_STANDARD;
use base64::Engine;
use clap::Parser;
use cli::identities::generate_identities;
use cli::Cli;
use futures::{stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Client;
use reth_rpc_layer::{Claims, JwtSecret};
use semaphore_rs::{identity::Identity, Field};
use crate::cli::i
mod cli;
mod transactions;

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
        cli::Commands::Send(args) => send_transactions(args).await?,
    }
    Ok(())
}

 // // Fetch an inclusion proof for the identity commitment
    // let client = Client::new();
    // let decoded = BASE64_STANDARD
    //     .decode(args.identity_secret)
    //     .expect("Failed to decode identity");

    // let trapdoor = &decoded[..32];
    // let nullifier = &decoded[32..];

    // // Create an Identity struct (assuming Field::from_bytes exists)
    // let identity = Identity {
    //     trapdoor: Field::from_be_slice(trapdoor),
    //     nullifier: Field::from_be_slice(nullifier),
    // };

    // let secret = JwtSecret::from_str(env!("AUTH")).unwrap();
    // info!(
    //     "Fetching inclusion proof for identity commitment: {}",
    //     identity.commitment()
    // );

    // let response = client
    //     .post(format!("{}/inclusionProof", STAGE_SEQUENCER_ORB))
    //     .json(&InclusionProofRequest {
    //         identity_commitment: identity.commitment(),
    //     })
    //     .send()
    //     .await?;

    // let response = response.error_for_status()?;
    // let response: InclusionProofResponse = response.json().await?;
    // let signer = PrivateKeySigner::from_str(&args.private_key)?;

    // // Set the Authorization header.
    // let mut headers = HeaderMap::new();
    // headers.insert(AUTHORIZATION, secret_to_bearer_header(&secret));

    // // Create the reqwest::Client with the AUTHORIZATION header.
    // let client_with_auth = Client::builder().default_headers(headers).build()?;

    // // Create the HTTP transport.
    // let http = Http::with_client(client_with_auth, args.rpc_url.parse()?);
    // let rpc_client = RpcClient::new(http, false);
    // let provider = ProviderBuilder::new()
    //     .wallet(EthereumWallet::from(signer.clone()))
    //     .on_client(rpc_client);

    // let pbh_transactions = transactions::generate_pbh_txs(
    //     &identity,
    //     response,
    //     signer.clone(),
    //     args.pbh_batch_size,
    //     args.nonce,
    // )
    // .await?;

    // let txs = transactions::generate_txs(
    //     signer,
    //     args.tx_batch_size,
    //     args.nonce + args.pbh_batch_size as u64,
    // )
    // .await?;

    // stream::iter(pbh_transactions.iter().zip(txs.iter()))
    //     .for_each_concurrent(1000, move |(pbh_tx, tx)| {
    //         info!("Sending PBH Transaction");
    //         let provider = provider.clone();
    //         async move {
    //             let (res0, res1) = tokio::join!(
    //                 provider.send_tx_envelope(pbh_tx.clone()),
    //                 provider.send_tx_envelope(tx.clone())
    //             );
    //             let receipt0 = res0
    //                 .expect("Failed to send PBH Transaction")
    //                 .get_receipt()
    //                 .await
    //                 .expect("Failed");
    //             info!(?receipt0, "Received tx receipt for PBH Transaction");
    //             let receipt1 = res1
    //                 .expect("Failed to send Non-PBH Transaction")
    //                 .get_receipt()
    //                 .await
    //                 .expect("Failed");
    //             info!(?receipt1, "Received tx receipt for Non-PBH Transaction");
    //         }
    //     })
    //     .await;

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
