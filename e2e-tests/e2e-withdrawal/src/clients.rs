//! Shared L1/L2 providers for withdrawal workflow commands.

use crate::{args::StepArgs, rpc, signer, types::StepError};
use alloy_network::EthereumWallet;
use alloy_primitives::Address;
use alloy_provider::{DynProvider, Provider, ProviderBuilder};

/// Wallet-backed L1 and L2 providers reused across workflow ticks.
pub struct Clients {
    /// L1 provider.
    pub l1: DynProvider,
    /// L2 provider.
    pub l2: DynProvider,
    /// Default signer address for the L2 wallet (needed by init).
    pub l2_address: Address,
}

impl Clients {
    /// Build both chain providers from step/run arguments.
    pub async fn from_step_args(args: &StepArgs) -> Result<Self, StepError> {
        let l1 = l1_provider(
            args.l1_args.l1_private_key.as_deref(),
            args.l1_args.l1_aws_kms_key_id.as_deref(),
            &args.l1_args.l1_rpc_endpoint,
        )
        .await?;
        let (l2, l2_address) = l2_provider(
            args.l2_private_key.as_deref(),
            args.l2_aws_kms_key_id.as_deref(),
            &args.l2_rpc_endpoint,
        )
        .await?;
        Ok(Self { l1, l2, l2_address })
    }
}

/// Build a wallet-backed L1 provider.
pub async fn l1_provider(
    private_key: Option<&str>,
    kms_key_id: Option<&str>,
    endpoint: &str,
) -> Result<DynProvider, StepError> {
    signed_provider(
        private_key,
        kms_key_id,
        endpoint,
        "L1_RPC_ENDPOINT",
        "L1_PRIVATE_KEY or L1_AWS_KMS_KEY_ID (exactly one)",
    )
    .await
    .map(|(provider, _)| provider)
}

/// Build a wallet-backed L2 provider and its default signer address.
pub async fn l2_provider(
    private_key: Option<&str>,
    kms_key_id: Option<&str>,
    endpoint: &str,
) -> Result<(DynProvider, Address), StepError> {
    signed_provider(
        private_key,
        kms_key_id,
        endpoint,
        "L2_RPC_ENDPOINT",
        "L2_PRIVATE_KEY or L2_AWS_KMS_KEY_ID (exactly one)",
    )
    .await
}

/// Build a read-only L2 provider (no signer).
pub fn l2_read_provider(endpoint: &str) -> Result<DynProvider, StepError> {
    Ok(ProviderBuilder::new()
        .connect_client(rpc::client(endpoint, "L2_RPC_ENDPOINT")?)
        .erased())
}

async fn signed_provider(
    private_key: Option<&str>,
    kms_key_id: Option<&str>,
    endpoint: &str,
    rpc_field: &'static str,
    signer_field: &'static str,
) -> Result<(DynProvider, Address), StepError> {
    let wallet = signer::wallet(private_key, kms_key_id, endpoint, rpc_field, signer_field).await?;
    let address = wallet.default_signer().address();
    Ok((provider_with_wallet(wallet, endpoint, rpc_field)?, address))
}

fn provider_with_wallet(
    wallet: EthereumWallet,
    endpoint: &str,
    rpc_field: &'static str,
) -> Result<DynProvider, StepError> {
    Ok(ProviderBuilder::new()
        .wallet(wallet)
        .connect_client(rpc::client(endpoint, rpc_field)?)
        .erased())
}
