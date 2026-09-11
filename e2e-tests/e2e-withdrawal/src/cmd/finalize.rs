use crate::{
    args::FinalizeArgs,
    bindings::{
        AnchorStateRegistry::AnchorStateRegistryInstance, OptimismPortal::OptimismPortalInstance,
        ProveWithdrawal,
    },
    storage,
};
use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::U256;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::{OptionExt, ensure};
use std::str::FromStr;

/// Run the `finalize` command.
pub async fn run(args: &FinalizeArgs) -> eyre::Result<()> {
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&args.l1_args.l1_private_key)?;
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&args.l1_args.l1_rpc_endpoint)
        .await?;
    // read ProveWithdrawal data (local path or s3://bucket/key)
    let prove_withdrawal: ProveWithdrawal = storage::read_json(&args.proven).await?;
    // check whether the withdrawal is finalizable:
    // 1. now - proven.timestamp > OptimismPortal::proofMaturityDelaySeconds()
    let optimism_portal = OptimismPortalInstance::new(args.optimism_portal, &l1_provider);
    let latest_block = l1_provider
        .get_block(BlockId::latest())
        .await?
        .ok_or_eyre("latest L1 block not found")?;
    let now = latest_block.header.timestamp;
    let proof_maturity_delay_seconds = optimism_portal.proofMaturityDelaySeconds().call().await?;
    ensure!(
        U256::from(now - prove_withdrawal.proven_at) > proof_maturity_delay_seconds,
        "proof maturity is not elapsed yet"
    );
    // 2. ASR.isGameClaimValid must return true
    let anchor_state_registry =
        AnchorStateRegistryInstance::new(args.anchor_state_registry, &l1_provider);
    let is_game_claim_valid = anchor_state_registry
        .isGameClaimValid(prove_withdrawal.game_addr)
        .call()
        .await?;
    ensure!(is_game_claim_valid, "game claim is not valid");
    // send the OptimismPortal::finalizeWithdrawal
    let pending_tx = optimism_portal
        .finalizeWithdrawalTransaction(prove_withdrawal.transaction)
        .send()
        .await?;
    let receipt = pending_tx.get_receipt().await?;
    ensure!(
        receipt.status(),
        "finalizeWithdrawalTransaction tx has not succeeded. Tx hash: {}",
        receipt.transaction_hash
    );
    ensure!(
        optimism_portal
            .finalizedWithdrawals(prove_withdrawal.hash)
            .call()
            .await?,
        "Portal did not persist the finalized withdrawal"
    );
    Ok(())
}
