use alloy_primitives::{Address, U256, utils::parse_ether};
use clap::Args;

/// Arguments for the `init` command.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// L2 rpc endpoint url.
    #[arg(long, env = "L2_RPC_ENDPOINT")]
    pub l2_rpc_endpoint: String,
    /// L2 private key.
    #[arg(long, env = "L2_PRIVATE_KEY")]
    pub l2_private_key: String,
    /// The amount of ETH you want to withdraw.
    #[arg(long, env = "ETH_VALUE", value_parser = parse_ether, default_value_t = U256::ZERO)]
    pub value: U256,
    /// Output location for the initiated withdrawal JSON (local path or `s3://bucket/key`).
    #[arg(
        long,
        env = "WITHDRAWAL_OUTPUT",
        default_value = "initiated_withdrawal.json"
    )]
    pub output: String,
}

/// Arguments for the `prove` command.
#[derive(Debug, Args)]
pub struct ProveArgs {
    /// L1 rpc endpoint url.
    #[arg(long, env = "L1_RPC_ENDPOINT")]
    pub l1_rpc_endpoint: String,
    /// L1 private key.
    #[arg(long, env = "L1_PRIVATE_KEY")]
    pub l1_private_key: String,
    /// L2 rpc endpoint url.
    #[arg(long, env = "L2_RPC_ENDPOINT")]
    pub l2_rpc_endpoint: String,
    /// DisputeGameFactory contract address.
    #[arg(long, env = "DISPUTE_GAME_FACTORY")]
    pub dispute_game_factory: Address,
    /// OptimismPortal contract address.
    #[arg(long, env = "OPTIMISM_PORTAL")]
    pub optimism_portal: Address,
    /// Input location for the initiated withdrawal JSON (local path or `s3://bucket/key`).
    #[arg(
        long,
        env = "WITHDRAWAL_INPUT",
        default_value = "initiated_withdrawal.json"
    )]
    pub input: String,
    /// Output location for the proven withdrawal JSON (local path or `s3://bucket/key`).
    #[arg(
        long,
        env = "WITHDRAWAL_OUTPUT",
        default_value = "prove_withdrawal.json"
    )]
    pub output: String,
}

/// Arguments for the `finalize` command.
#[derive(Debug, Args)]
pub struct FinalizeArgs {
    /// L1 rpc endpoint url.
    #[arg(long, env = "L1_RPC_ENDPOINT")]
    pub l1_rpc_endpoint: String,
    /// L1 private key.
    #[arg(long, env = "L1_PRIVATE_KEY")]
    pub l1_private_key: String,
    /// AnchorStateRegistry contract address.
    #[arg(long, env = "ANCHOR_STATE_REGISTRY")]
    pub anchor_state_registry: Address,
    /// OptimismPortal contract address.
    #[arg(long, env = "OPTIMISM_PORTAL")]
    pub optimism_portal: Address,
    /// Input location for the proven withdrawal JSON (local path or `s3://bucket/key`).
    #[arg(
        long,
        env = "WITHDRAWAL_INPUT",
        default_value = "prove_withdrawal.json"
    )]
    pub input: String,
}
