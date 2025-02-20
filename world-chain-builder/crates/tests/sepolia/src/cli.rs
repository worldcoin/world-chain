use clap::{Parser, ValueEnum};
use reth_rpc_layer::JwtSecret;

pub mod identities;
pub mod transactions;

/// A CLI utility for load testing WorldChain Sepolia PBH
#[derive(Debug, Clone, Parser)]
#[clap(version, about)]
pub struct Cli {
    #[clap(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Parser)]
pub enum Commands {
    /// Command generate Test identities inserted into the signup sequencer.
    Generate(GenerateArgs),
    /// Command to create a bundle of PBH transactions, or PBH
    /// UserOperations on specified identities.
    Bundle(BundleArgs),
    /// Command to send a batch of test transactions, or UserOperations
    /// to the WC Sepolia Testnet.
    Send(SendArgs),
}

#[derive(Debug, Clone, Parser)]
pub struct GenerateArgs {
    /// The file path to write the generated identities.
    #[clap(long, short, default_value = "test_identities.json")]
    pub path: String,
    /// The URL to the target signup sequencer.
    #[clap(long, short, default_value = "https://signup-orb-ethereum.stage-crypto.worldcoin.dev")]
    pub sequencer_url: String,
    /// The Username to authenticate with the signup sequencer.
    #[clap(long, short)]
    pub username: String,
    /// The Password to authenticate with the signup sequencer.
    #[clap(long)]
    pub password: String,
    /// The number of identities to generate.
    #[clap(long, short, default_value_t = 1)]
    pub count: u64,
}

#[derive(Debug, Clone, Parser)]
pub struct BundleArgs {
    /// The file path to read the identities from.
    #[clap(long, default_value = "test_identities.json")]
    pub identities_path: String,
    /// The URL to the target signup sequencer.
    #[clap(long, short, default_value = "https://signup-orb-ethereum.stage-crypto.worldcoin.dev")]
    pub sequencer_url: String,
    /// The Chain ID
    #[clap(long, default_value_t = 4801)]
    pub chain_id: u64,
    /// The `PBHEntryPoint` address
    #[clap(long, default_value = "0x7AcDc12cbCba53E1ea2206844D0A8cCb6f3B08fB")]
    pub pbh_entry_point: String,
    /// The file path to write the generated bundle.
    #[clap(long, short, default_value = "pbh_bundle.json")]
    pub bundle_path: String,
    /// The number of PBH transactions to generate per identity.
    #[clap(long, default_value_t = 1)]
    pub pbh_batch_size: u8,
    /// The number of Non-PBH transactions to generate.
    #[clap(long, default_value_t = 0)]
    pub tx_batch_size: u8,
    /// The private key signer for the transactions or UserOperations.
    #[clap(long)]
    pub private_key: String,
    /// The nonce for the wallet.
    #[clap(long, short, default_value_t = 0)]
    pub nonce: u64,
    /// Whether to create PBH transactions or UserOperations.
    #[clap(long, default_value = "transaction")]
    pub tx_type: TxType,
}

#[derive(Debug, Clone, Parser)]
pub struct SendArgs {
    /// The Transaction Type to send to the WC Sepolia Testnet.
    #[clap(long, default_value = "transaction")]
    pub tx_type: TxType,
    /// The file path to write the generated bundle.
    #[clap(long, short, default_value = "pbh_bundle.json")]
    pub bundle_path: String,
    /// The RPC URL for WC Sepolia Testnet.
    #[clap(long, short, required = true)]
    pub rpc_url: String,

    /// JWT Secret authorization in the headers.
    // TODO:
    #[clap(long, short)] 
    pub auth: Option<JwtSecret>,
    
}

#[derive(Debug, Clone, ValueEnum)]
pub enum TxType {
    Transaction,
    UserOperation,
}