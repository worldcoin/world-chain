use alloy_primitives::{uint, Address, U256};
use clap::{Parser, ValueEnum};
use reth_rpc_layer::JwtSecret;

pub mod identities;
pub mod transactions;

/// A CLI utility for load testing PBH transactions.
///
/// The CLI consists of three stages:
///  - `generate`: Command to generate Test identities, and insert them into the signup sequencer.
///  - `bundle`: Command to create a bundle of PBH transactions, or PBH UserOperations on specified identities.
///  - `send`: Command to send a batch of test transactions, or UserOperations.
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
    /// Command to send a batch of test transactions, or UserOperations
    /// to the WC Sepolia Testnet.
    SendAA(SendAAArgs),
    /// Command to stake a safe in the Entrypoint.
    StakeAA(StakeAAArgs),
    /// Command to send a batch of PBH bundles with invalid proofs but valid nullifiers so it would successfully execute on-chain
    /// This is used to stress test peer banning as it would fail proof validation in the builder but succeed on-chain
    /// It sends 1 UO per bundle from the same safe sequentially
    SendInvalidProofPBH(SendInvalidProofPBHArgs),
    /// Load test command to stress test the bundler
    LoadTest(LoadTestArgs),
}

#[derive(Debug, Clone, Parser)]
pub struct LoadTestArgs {
    /// The RPC URL
    #[clap(long, env = "RPC_URL")]
    pub rpc_url: String,
    /// The path to a json holding information pertaining to the Safes, and Module
    #[clap(long, env = "CONFIG_PATH", default_value = "load_test_config.json")]
    pub config_path: String,
    #[clap(long, env = "CONCURRENCY", default_value_t = 100)]
    pub concurrency: usize,
    #[clap(long, env = "TRANSACTION_COUNT", default_value_t = 1)]
    pub transaction_count: usize,
}

#[derive(Debug, Clone, Parser)]
pub struct GenerateArgs {
    /// The file path to write the generated identities.
    #[clap(long, short, default_value = "test_identities.json")]
    pub path: String,
    /// The URL to the target signup sequencer.
    #[clap(
        long,
        short,
        default_value = "https://signup-orb-ethereum.stage-crypto.worldcoin.dev"
    )]
    pub sequencer_url: String,
    /// The Username to authenticate with the signup sequencer.
    #[clap(long, short, required = true)]
    pub username: String,
    /// The Password to authenticate with the signup sequencer.
    #[clap(long, required = true)]
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
    #[clap(
        long,
        short,
        default_value = "https://signup-orb-ethereum.stage-crypto.worldcoin.dev"
    )]
    pub sequencer_url: String,
    /// The Chain ID of the network.
    #[clap(long, default_value_t = 4801)]
    pub chain_id: u64,
    /// The `PBHEntryPoint` address
    #[clap(long, default_value = "0x0000000000A21818Ee9F93BB4f2AAad305b5397C")]
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
    /// The private key signer for transactions or UserOperations.
    #[clap(long, required = true)]
    pub pbh_private_key: String,
    /// The private key signer for Non-PBH transactions.
    #[clap(long, required = true)]
    pub std_private_key: String,
    /// The nonce for the wallet.
    #[clap(long, default_value_t = 0)]
    pub pbh_nonce: u64,
    /// The nonce for the wallet.
    #[clap(long, default_value_t = 0)]
    pub std_nonce: u64,
    /// Whether to create PBH transactions or UserOperations.
    #[clap(long, default_value = "transaction")]
    pub tx_type: TxType,
    /// Address of the Safe to execute UserOperation's on.
    #[clap(long, required_if_eq("tx_type", "user-operation"))]
    pub safe: Option<Address>,
    /// Address of the Module to execute UserOperation's on.
    #[clap(long, required_if_eq("tx_type", "user-operation"))]
    pub module: Option<Address>,
}

#[derive(Debug, Clone, Parser)]
pub struct SendArgs {
    /// The Transaction Type to send to the WC Sepolia Testnet.
    #[clap(long, default_value = "transaction")]
    pub tx_type: TxType,
    /// The file path to write the generated bundle.
    #[clap(long, default_value = "pbh_bundle.json")]
    pub bundle_path: String,
    /// The RPC URL
    /// The RPC URL
    #[clap(
        long,
        short,
        default_value = "https://worldchain-sepolia.g.alchemy.com/public"
    )]
    pub rpc_url: String,
    /// JWT Secret authorization in the headers.
    #[clap(long, short)]
    pub auth: Option<JwtSecret>,
}

#[derive(Debug, Clone, Parser)]
pub struct SendAAArgs {
    /// The RPC URL
    /// The RPC URL
    #[clap(
        long,
        short,
        default_value = "https://worldchain-sepolia.g.alchemy.com/public"
    )]
    pub rpc_url: String,
    /// The file path to read the identities from.
    #[clap(long, default_value = "test_identities.json")]
    pub identities_path: String,
    /// The URL to the target signup sequencer.
    #[clap(
        long,
        short,
        default_value = "https://signup-orb-ethereum.stage-crypto.worldcoin.dev"
    )]
    pub sequencer_url: String,
    /// The Chain ID of the network.
    #[clap(long, default_value_t = 4801)]
    pub chain_id: u64,
    /// The `PBHEntryPoint` address
    #[clap(long, default_value = "0x0000000000A21818Ee9F93BB4f2AAad305b5397C")]
    pub pbh_entry_point: String,
    /// The number of PBH transactions to generate per identity.
    #[clap(long, default_value_t = 1)]
    pub pbh_batch_size: u8,
    /// The private key signer.
    #[clap(long, required = true)]
    pub pbh_private_key: String,
    /// The nonce for the wallet.
    #[clap(long)]
    pub pbh_nonce: Option<u64>,
    // Address of the Safe to execute UserOperation's on.
    #[clap(long, required = true)]
    pub safe: Address,
    /// Address of the Module to execute UserOperation's on.
    #[clap(long, required = true)]
    pub module: Address,
    /// The number of concurrent requests to send.
    #[clap(long, default_value_t = 100)]
    pub concurrency: usize,
}

#[derive(Debug, Clone, Parser)]
pub struct StakeAAArgs {
    /// The RPC URL
    /// The RPC URL
    #[clap(
        long,
        short,
        default_value = "https://worldchain-sepolia.g.alchemy.com/public"
    )]
    pub rpc_url: String,
    /// The Chain ID of the network.
    #[clap(long, default_value_t = 4801)]
    pub chain_id: u64,
    /// The value to stake in the EntryPoint.
    #[clap(long, default_value_t = uint!(100000000000000000_U256))]
    pub stake_amount: U256,
    /// The private key signer.
    #[clap(long, required = true)]
    pub pbh_private_key: String,
    // Address of the Safe to execute UserOperation's on.
    #[clap(long, required = true)]
    pub safe: Address,
    /// Address of the Module to execute UserOperation's on.
    #[clap(long, required = true)]
    pub module: Address,
}

#[derive(Debug, Clone, Parser)]
pub struct SendInvalidProofPBHArgs {
    /// The RPC URL
    #[clap(
        long,
        short,
        default_value = "https://worldchain-sepolia.g.alchemy.com/public"
    )]
    pub rpc_url: String,
    /// The file path to read the identities from.
    #[clap(long, default_value = "test_identities.json")]
    pub identities_path: String,
    /// The URL to the target signup sequencer.
    #[clap(
        long,
        short,
        default_value = "https://signup-orb-ethereum.stage-crypto.worldcoin.dev"
    )]
    pub sequencer_url: String,
    /// The Chain ID of the network.
    #[clap(long, default_value_t = 4801)]
    pub chain_id: u64,
    /// The `PBHEntryPoint` address
    #[clap(long, default_value = "0x0000000000A21818Ee9F93BB4f2AAad305b5397C")]
    pub pbh_entry_point: String,
    /// The starting pbh nonce for the wallet to increment from
    #[clap(long)]
    pub pbh_nonce: Option<u64>,
    /// The private key signer.
    #[clap(long, required = true)]
    pub pbh_private_key: String,
    // Address of the Safe to execute UserOperation's on.
    #[clap(long, required = true)]
    pub safe: Address,
    /// Address of the Module to execute UserOperation's on.
    #[clap(long, required = true)]
    pub module: Address,
    /// The number of sequential transactions to send.
    #[clap(long, default_value_t = 1)]
    pub transaction_count: usize,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum TxType {
    Transaction,
    UserOperation,
}
