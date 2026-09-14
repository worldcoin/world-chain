use crate::types::StepError;
use alloy_primitives::{Address, U256, utils::parse_ether};
use alloy_signer_local::PrivateKeySigner;
use clap::Args;
use std::{
    path::{Component, Path, PathBuf},
    str::FromStr,
};

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
    /// Validate the complete workflow before any transaction is submitted.
    pub fn validate(&self) -> Result<(), StepError> {
        self.l1_args.validate()?;
        validate_key(&self.l2_private_key, "L2_PRIVATE_KEY")?;
        rpc_url(&self.l2_rpc_endpoint, "L2_RPC_ENDPOINT")?;
        validate_handoffs(&self.initiated, &self.proven)
    }

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

impl L1Args {
    fn validate(&self) -> Result<(), StepError> {
        validate_key(&self.l1_private_key, "L1_PRIVATE_KEY")?;
        rpc_url(&self.l1_rpc_endpoint, "L1_RPC_ENDPOINT")?;
        Ok(())
    }
}

impl InitArgs {
    pub fn validate(&self) -> Result<(), StepError> {
        validate_key(&self.l2_private_key, "L2_PRIVATE_KEY")?;
        rpc_url(&self.l2_rpc_endpoint, "L2_RPC_ENDPOINT")?;
        handoff_identity(&self.initiated, "WITHDRAWAL_INITIATED")?;
        Ok(())
    }
}

impl ProveArgs {
    pub fn validate(&self) -> Result<(), StepError> {
        self.l1_args.validate()?;
        rpc_url(&self.l2_rpc_endpoint, "L2_RPC_ENDPOINT")?;
        validate_handoffs(&self.initiated, &self.proven)
    }
}

impl FinalizeArgs {
    pub fn validate(&self) -> Result<(), StepError> {
        self.l1_args.validate()?;
        handoff_identity(&self.proven, "WITHDRAWAL_PROVEN")?;
        Ok(())
    }
}

fn validate_key(value: &str, field: &'static str) -> Result<(), StepError> {
    PrivateKeySigner::from_str(value).map_err(|_| StepError::InvalidConfiguration { field })?;
    Ok(())
}

/// Parse only supported HTTP(S) endpoints without exposing credentials in errors.
pub fn rpc_url(value: &str, field: &'static str) -> Result<reqwest::Url, StepError> {
    let url = reqwest::Url::parse(value).map_err(|_| StepError::InvalidConfiguration { field })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(StepError::InvalidConfiguration { field });
    }
    Ok(url)
}

fn validate_handoffs(initiated: &str, proven: &str) -> Result<(), StepError> {
    if handoff_identity(initiated, "WITHDRAWAL_INITIATED")?
        == handoff_identity(proven, "WITHDRAWAL_PROVEN")?
    {
        return Err(StepError::InvalidConfiguration {
            field: "WITHDRAWAL_INITIATED and WITHDRAWAL_PROVEN must be distinct",
        });
    }
    Ok(())
}

#[derive(PartialEq)]
enum HandoffIdentity {
    S3(String),
    Local(PathBuf),
}

fn handoff_identity(value: &str, field: &'static str) -> Result<HandoffIdentity, StepError> {
    let invalid = || StepError::InvalidConfiguration { field };
    if let Some(rest) = value.strip_prefix("s3://") {
        let (bucket, key) = rest.split_once('/').ok_or_else(invalid)?;
        if bucket.is_empty() || key.is_empty() {
            return Err(invalid());
        }
        // S3 keys are literal: do not normalize their slashes or dot segments.
        return Ok(HandoffIdentity::S3(value.to_owned()));
    }
    if value.trim().is_empty() || value.contains("://") || value.contains('\0') {
        return Err(invalid());
    }
    let path = Path::new(value);
    let mut resolved = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().map_err(|_| invalid())?
    };
    // Resolve existing ancestors as well as lexical aliases for not-yet-created
    // handoffs, so ./file and symlinked parents cannot hide a collision.
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other.as_os_str());
                match resolved.canonicalize() {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(invalid()),
                }
            }
        }
    }
    if resolved.file_name().is_none() || resolved.is_dir() {
        return Err(invalid());
    }
    Ok(HandoffIdentity::Local(resolved))
}
