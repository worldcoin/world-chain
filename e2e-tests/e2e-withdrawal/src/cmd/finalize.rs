use crate::{
    args::FinalizeArgs,
    bindings::{
        AnchorStateRegistry::AnchorStateRegistryInstance, IMultiProofGame::IMultiProofGameInstance,
        OptimismPortal::OptimismPortalInstance, ProveWithdrawal,
    },
    storage,
    types::{
        FinalizedOutcome, GameBlockedReason, StepError, StepOutcome, StepStage, WaitingOutcome,
        WaitingReason,
    },
};
use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::ruint::FromUintError;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::{OptionExt, WrapErr, eyre};
use std::str::FromStr;

/// Run the `finalize` command.
pub async fn run(args: &FinalizeArgs) -> Result<StepOutcome, StepError> {
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&args.l1_args.l1_private_key).map_err(|_| {
        StepError::InvalidConfiguration {
            field: "L1_PRIVATE_KEY",
        }
    })?;
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect_reqwest(
            super::rpc_client()?,
            args.l1_args
                .l1_rpc_endpoint
                .parse()
                .map_err(|_| StepError::InvalidConfiguration {
                    field: "L1_RPC_ENDPOINT",
                })?,
        );
    // read ProveWithdrawal data (local path or s3://bucket/key)
    let prove_withdrawal: ProveWithdrawal = storage::read_json(&args.proven).await?;
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
    // now - proven.timestamp > OptimismPortal::proofMaturityDelaySeconds()
    let latest_block = l1_provider
        .get_block(BlockId::latest())
        .await
        .map_err(|err| StepError::Generic(err.into()))?
        .ok_or_eyre("latest L1 block not found")?;
    let now = latest_block.header.timestamp;
    let block = BlockId::hash(latest_block.header.hash);
    let proof_maturity_delay_seconds = optimism_portal
        .proofMaturityDelaySeconds()
        .block(block)
        .call()
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    let proof_maturity_delay_seconds = proof_maturity_delay_seconds
        .try_into()
        .map_err(|err: FromUintError<u64>| StepError::Generic(err.into()))?;
    if now < prove_withdrawal.proven_at {
        return Err(StepError::InvalidState {
            stage: StepStage::Finalize,
            message: "proven timestamp is later than the latest L1 block",
        });
    }
    // the portal requires elapsed time to be strictly greater than the delay.
    let eligible_at = prove_withdrawal
        .proven_at
        .checked_add(proof_maturity_delay_seconds)
        .and_then(|timestamp| timestamp.checked_add(1))
        .ok_or(StepError::InvalidState {
            stage: StepStage::Finalize,
            message: "proof maturity timestamp overflows u64",
        })?;
    // detect blocked games even while proof maturity is still pending. All game
    // checks use the same block hash so a changing head cannot mix game states.
    let game_waiting = check_game(&l1_provider, args, &prove_withdrawal, block).await?;
    if now < eligible_at {
        let waiting_outcome = WaitingOutcome {
            withdrawal_hash: prove_withdrawal.hash,
            stage: StepStage::Finalize,
            waiting_reason: WaitingReason::ProofMaturityNotElapsed { eligible_at },
        };
        return Ok(StepOutcome::Waiting(waiting_outcome));
    }
    if let Some(waiting_reason) = game_waiting {
        let waiting_outcome = WaitingOutcome {
            withdrawal_hash: prove_withdrawal.hash,
            stage: StepStage::Finalize,
            waiting_reason,
        };
        return Ok(StepOutcome::Waiting(waiting_outcome));
    }
    // send the OptimismPortal::finalizeWithdrawal
    let pending_tx = optimism_portal
        .finalizeWithdrawalTransaction(prove_withdrawal.transaction)
        .send()
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    let tx_hash = *pending_tx.tx_hash();
    let receipt =
        super::receipt_with_deadline(tx_hash, StepStage::Finalize, pending_tx.get_receipt())
            .await?;
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
        withdrawal_hash: prove_withdrawal.hash,
        tx_hash: receipt.transaction_hash,
    }))
}

/// Distinguish expected game progress from conditions that require intervention.
async fn check_game<P: Provider>(
    provider: &P,
    args: &FinalizeArgs,
    withdrawal: &ProveWithdrawal,
    block: BlockId,
) -> Result<Option<WaitingReason>, StepError> {
    let registry = AnchorStateRegistryInstance::new(args.anchor_state_registry, provider);
    let address = withdrawal.game_addr;
    let blocked = |reason| StepError::GameBlocked {
        withdrawal_hash: withdrawal.hash,
        game_address: address,
        reason,
    };

    if registry
        .isGameClaimValid(address)
        .block(block)
        .call()
        .await
        .wrap_err("L1 RPC: checking game claim validity")?
    {
        return Ok(None);
    }
    if registry
        .paused()
        .block(block)
        .call()
        .await
        .wrap_err("L1 RPC: checking system pause")?
    {
        return Err(blocked(GameBlockedReason::SystemPaused));
    }
    if !registry
        .isGameRegistered(address)
        .block(block)
        .call()
        .await
        .wrap_err("L1 RPC: checking game registration")?
    {
        return Err(blocked(GameBlockedReason::NotRegistered));
    }
    if registry
        .isGameBlacklisted(address)
        .block(block)
        .call()
        .await
        .wrap_err("L1 RPC: checking game blacklist")?
    {
        return Err(blocked(GameBlockedReason::Blacklisted));
    }
    if registry
        .isGameRetired(address)
        .block(block)
        .call()
        .await
        .wrap_err("L1 RPC: checking game retirement")?
    {
        return Err(blocked(GameBlockedReason::Retired));
    }
    if !registry
        .isGameRespected(address)
        .block(block)
        .call()
        .await
        .wrap_err("L1 RPC: checking respected game type")?
    {
        return Err(blocked(GameBlockedReason::NotRespected));
    }

    let game = IMultiProofGameInstance::new(address, provider);
    match game
        .status()
        .block(block)
        .call()
        .await
        .wrap_err("L1 RPC: checking game status")?
    {
        // GameStatus: IN_PROGRESS = 0, CHALLENGER_WINS = 1, DEFENDER_WINS = 2.
        0 => {
            return Ok(Some(WaitingReason::GameUnresolved {
                game_address: address,
            }));
        }
        1 => return Err(blocked(GameBlockedReason::ChallengerWon)),
        2 => {}
        _ => {
            return Err(StepError::InvalidState {
                stage: StepStage::Finalize,
                message: "unrecognized dispute game status",
            });
        }
    }
    if !registry
        .isGameFinalized(address)
        .block(block)
        .call()
        .await
        .wrap_err("L1 RPC: checking game finality delay")?
    {
        return Ok(Some(WaitingReason::GameFinalityDelay {
            game_address: address,
        }));
    }
    // Do not turn an unexplained rejection into successful waiting.
    Err(blocked(GameBlockedReason::InvalidClaim))
}
