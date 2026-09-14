use crate::{
    args::FinalizeArgs,
    bindings::{
        AnchorStateRegistry::AnchorStateRegistryInstance, OptimismPortal::OptimismPortalInstance,
        ProveWithdrawal,
    },
    storage,
    types::{FinalizedOutcome, StepError, StepOutcome, StepStage, WaitingOutcome, WaitingReason},
};
use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::ruint::FromUintError;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::{OptionExt, eyre};
use std::str::FromStr;

/// Run the `finalize` command.
pub async fn run(args: &FinalizeArgs) -> Result<StepOutcome, StepError> {
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&args.l1_args.l1_private_key)
        .map_err(|err| StepError::Generic(err.into()))?;
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&args.l1_args.l1_rpc_endpoint)
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    // read ProveWithdrawal data (local path or s3://bucket/key)
    let prove_withdrawal: ProveWithdrawal = storage::read_json(&args.proven)
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    // already finalized on-chain, treat as success so step can retry handoff cleanup.
    let optimism_portal = OptimismPortalInstance::new(args.optimism_portal, &l1_provider);
    if optimism_portal
        .finalizedWithdrawals(prove_withdrawal.hash)
        .call()
        .await
        .map_err(|err| StepError::Generic(err.into()))?
    {
        return Ok(StepOutcome::Finalized(FinalizedOutcome::AlreadyFinalized {
            withdrawal_hash: prove_withdrawal.hash,
        }));
    }
    // check whether the withdrawal is finalizable:
    // 1. now - proven.timestamp > OptimismPortal::proofMaturityDelaySeconds()
    let latest_block = l1_provider
        .get_block(BlockId::latest())
        .await
        .map_err(|err| StepError::Generic(err.into()))?
        .ok_or_eyre("latest L1 block not found")
        .map_err(|err| StepError::Generic(err.into()))?;
    let now = latest_block.header.timestamp;
    let proof_maturity_delay_seconds = optimism_portal
        .proofMaturityDelaySeconds()
        .call()
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    let proof_maturity_delay_seconds = proof_maturity_delay_seconds
        .try_into()
        .map_err(|err: FromUintError<u64>| StepError::Generic(err.into()))?;
    if now - prove_withdrawal.proven_at <= proof_maturity_delay_seconds {
        let eligible_at = prove_withdrawal.proven_at + proof_maturity_delay_seconds;
        let waiting_outcome = WaitingOutcome {
            withdrawal_hash: prove_withdrawal.hash,
            stage: StepStage::Finalize,
            waiting_reason: WaitingReason::ProofMaturityNotElapsed { eligible_at },
        };
        return Ok(StepOutcome::Waiting(waiting_outcome));
    }
    // 2. ASR.isGameClaimValid must return true
    let anchor_state_registry =
        AnchorStateRegistryInstance::new(args.anchor_state_registry, &l1_provider);
    let is_game_claim_valid = anchor_state_registry
        .isGameClaimValid(prove_withdrawal.game_addr)
        .call()
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    if !is_game_claim_valid {
        let waiting_outcome = WaitingOutcome {
            withdrawal_hash: prove_withdrawal.hash,
            stage: StepStage::Finalize,
            waiting_reason: WaitingReason::GameClaimIsNotValid {
                game_address: prove_withdrawal.game_addr,
            },
        };
        return Ok(StepOutcome::Waiting(waiting_outcome));
    }
    // send the OptimismPortal::finalizeWithdrawal
    let pending_tx = optimism_portal
        .finalizeWithdrawalTransaction(prove_withdrawal.transaction)
        .send()
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    let receipt = pending_tx
        .get_receipt()
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    if !receipt.status() {
        return Err(StepError::Generic(eyre!(
            "finalizeWithdrawalTransaction tx has not succeeded. Tx hash: {}",
            receipt.transaction_hash
        )));
    }
    let is_finalized = optimism_portal
        .finalizedWithdrawals(prove_withdrawal.hash)
        .call()
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    if !is_finalized {
        return Err(StepError::Generic(eyre!(
            "Portal did not persist the finalized withdrawal"
        )));
    }
    Ok(StepOutcome::Finalized(FinalizedOutcome::Finalized {
        witdrawal_hash: prove_withdrawal.hash,
        tx_hash: receipt.transaction_hash,
    }))
}
