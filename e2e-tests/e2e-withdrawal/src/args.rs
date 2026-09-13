use alloy_primitives::{Address, U256, utils::parse_ether};
use clap::Args;

/// Arguments for the L1.
#[derive(Debug, Args, Clone)]
pub struct L1Args {
    /// L1 rpc endpoint url.
    #[arg(long, env = "L1_RPC_ENDPOINT")]
    pub l1_rpc_endpoint: String,
    /// L1 private key.
    #[arg(long, env = "L1_PRIVATE_KEY")]
    pub l1_private_key: String,
}

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
    /// Location for the initiated withdrawal JSON (local path or `s3://bucket/key`).
    #[arg(
        long,
        env = "WITHDRAWAL_INITIATED",
        default_value = "initiated_withdrawal.json"
    )]
    pub initiated: String,
}

/// Arguments for the `prove` command.
#[derive(Debug, Args)]
pub struct ProveArgs {
    /// L1 args.
    #[command(flatten)]
    pub l1_args: L1Args,
    /// L2 rpc endpoint url.
    #[arg(long, env = "L2_RPC_ENDPOINT")]
    pub l2_rpc_endpoint: String,
    /// DisputeGameFactory contract address.
    #[arg(long, env = "DISPUTE_GAME_FACTORY")]
    pub dispute_game_factory: Address,
    /// OptimismPortal contract address.
    #[arg(long, env = "OPTIMISM_PORTAL")]
    pub optimism_portal: Address,
    /// Location for the initiated withdrawal JSON (local path or `s3://bucket/key`).
    #[arg(
        long,
        env = "WITHDRAWAL_INITIATED",
        default_value = "initiated_withdrawal.json"
    )]
    pub initiated: String,
    /// Location for the proven withdrawal JSON (local path or `s3://bucket/key`).
    #[arg(
        long,
        env = "WITHDRAWAL_PROVEN",
        default_value = "prove_withdrawal.json"
    )]
    pub proven: String,
}

/// Arguments for the `finalize` command.
#[derive(Debug, Args)]
pub struct FinalizeArgs {
    /// L1 args.
    #[command(flatten)]
    pub l1_args: L1Args,
    /// AnchorStateRegistry contract address.
    #[arg(long, env = "ANCHOR_STATE_REGISTRY")]
    pub anchor_state_registry: Address,
    /// OptimismPortal contract address.
    #[arg(long, env = "OPTIMISM_PORTAL")]
    pub optimism_portal: Address,
    /// Location for the proven withdrawal JSON (local path or `s3://bucket/key`).
    #[arg(
        long,
        env = "WITHDRAWAL_PROVEN",
        default_value = "prove_withdrawal.json"
    )]
    pub proven: String,
}

/// Arguments for the `step` command.
#[derive(Debug, Args)]
pub struct StepArgs {
    /// L1 args.
    #[command(flatten)]
    pub l1_args: L1Args,
    /// L2 rpc endpoint url.
    #[arg(long, env = "L2_RPC_ENDPOINT")]
    pub l2_rpc_endpoint: String,
    /// L2 private key.
    #[arg(long, env = "L2_PRIVATE_KEY")]
    pub l2_private_key: String,
    /// The amount of ETH you want to withdraw.
    #[arg(long, env = "ETH_VALUE", value_parser = parse_ether, default_value_t = U256::ZERO)]
    pub value: U256,
    /// DisputeGameFactory contract address.
    #[arg(long, env = "DISPUTE_GAME_FACTORY")]
    pub dispute_game_factory: Address,
    /// OptimismPortal contract address.
    #[arg(long, env = "OPTIMISM_PORTAL")]
    pub optimism_portal: Address,
    /// AnchorStateRegistry contract address.
    #[arg(long, env = "ANCHOR_STATE_REGISTRY")]
    pub anchor_state_registry: Address,
    /// Durable location for the initiated-withdrawal JSON.
    #[arg(
        long,
        env = "WITHDRAWAL_INITIATED",
        default_value = "initiated_withdrawal.json"
    )]
    pub initiated: String,
    /// Durable location for the proven-withdrawal JSON.
    #[arg(
        long,
        env = "WITHDRAWAL_PROVEN",
        default_value = "prove_withdrawal.json"
    )]
    pub proven: String,
}

impl StepArgs {
    /// Convert into `InitArgs`
    pub fn to_init(&self) -> InitArgs {
        InitArgs {
            l2_rpc_endpoint: self.l2_rpc_endpoint.clone(),
            l2_private_key: self.l2_private_key.clone(),
            value: self.value,
            initiated: self.initiated.clone(),
        }
    }

    /// Convert into `ProveArgs`
    pub fn to_prove(&self) -> ProveArgs {
        ProveArgs {
            l1_args: self.l1_args.clone(),
            l2_rpc_endpoint: self.l2_rpc_endpoint.clone(),
            dispute_game_factory: self.dispute_game_factory,
            optimism_portal: self.optimism_portal,
            initiated: self.initiated.clone(),
            proven: self.proven.clone(),
        }
    }

    /// Convert into `FinalizeArgs`
    pub fn to_finalize(&self) -> FinalizeArgs {
        FinalizeArgs {
            l1_args: self.l1_args.clone(),
            anchor_state_registry: self.anchor_state_registry,
            optimism_portal: self.optimism_portal,
            proven: self.proven.clone(),
        }
    }
}
