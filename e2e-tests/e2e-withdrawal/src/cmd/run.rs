use crate::{
    args::StepArgs,
    bindings::{
        IDisputeGameFactory::IDisputeGameFactoryInstance, IMultiProofGame::IMultiProofGameInstance,
        InitiatedWithdrawal, L2ToL1MessagePasser, OptimismPortal, ProveWithdrawal,
        WithdrawalTransaction,
    },
    cmd::step,
    storage,
    types::{StepError, StepOutcome, StepStage},
};
use alloy_consensus::{BlockHeader, Transaction};
use alloy_eips::BlockId;
use alloy_primitives::{B256, U256, ruint::FromUintError};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_sol_types::SolCall;
use backoff::{Error as BackoffError, ExponentialBackoff, future::retry_notify};
use eyre::eyre::OptionExt;
use serde::Serialize;
use std::time::Duration;

/// Run the `run` command.
pub async fn run(args: &StepArgs) -> Result<StepOutcome, StepError> {
    loop {
        let step_result = step::run(&args).await;
        if let Ok(step_outcome) = step_result {
            match step_outcome {
                StepOutcome::Initiated(initiated_outcome) => {
                    tracing::info!(?initiated_outcome);
                }
                StepOutcome::Proven(proven_outcome) => {
                    tracing::info!(?proven_outcome);
                }
                StepOutcome::Finalized(finalized_outcome) => {
                    tracing::info!(?finalized_outcome);
                }
                StepOutcome::Waiting(waiting_outcome) => {
                    tracing::info!(?waiting_outcome);
                }
            }
        } else {
            // check if it's a retryable error:
            // - if it is, then retry `step`

            // safe unwrap because we've just checked above
            let step_error = step_result.unwrap_err();
            tracing::error!(?step_error);
            if step_error.is_retryable() {
                // sleep and continue
                tokio::time::sleep(Duration::from_secs(12)).await;
                continue;
            }

            // if error is `PersistenceAfterTransaction` we need to gether the tx receipt,
            // recreate the needed data, save it and then continue the loop
            if let StepError::PersistenceAfterTransaction {
                transaction_hash,
                stage,
                source: _,
            } = &step_error
            {
                match stage {
                    StepStage::Init => {
                        let l2_provider = ProviderBuilder::new()
                            .connect(&args.l2_rpc_endpoint)
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?;
                        let receipt = l2_provider
                            .get_transaction_receipt(*transaction_hash)
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?
                            .ok_or_eyre("init withdrawal receipt missing")?;
                        let l2_block = receipt
                            .block_number
                            .ok_or_eyre("withdrawal receipt missing L2 block number")?;
                        let message = receipt
                            .logs()
                            .iter()
                            .find_map(|log| {
                                log.log_decode_validate::<L2ToL1MessagePasser::MessagePassed>()
                                    .ok()
                            })
                            .ok_or_eyre("withdrawal receipt missing MessagePassed event")?;
                        let message = message.data();
                        let initiated_withdrawal = InitiatedWithdrawal {
                            transaction: WithdrawalTransaction {
                                nonce: message.nonce,
                                sender: message.sender,
                                target: message.target,
                                value: message.value,
                                gasLimit: message.gasLimit,
                                data: message.data.clone(),
                            },
                            hash: message.withdrawalHash,
                            l2_block,
                        };
                        write_json_with_backoff(
                            &args.initiated,
                            &initiated_withdrawal,
                            *transaction_hash,
                            StepStage::Init,
                        )
                        .await?;
                        tokio::time::sleep(Duration::from_secs(12)).await;
                        continue;
                    }
                    StepStage::Prove => {
                        let l1_provider = ProviderBuilder::new()
                            .connect(&args.l1_args.l1_rpc_endpoint)
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?;
                        // initiated handoff is still present; proven write is what failed
                        let initiated_withdrawal: InitiatedWithdrawal =
                            storage::read_json(&args.initiated)
                                .await
                                .map_err(|err| StepError::Generic(err.into()))?;
                        let receipt = l1_provider
                            .get_transaction_receipt(*transaction_hash)
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?
                            .ok_or_eyre("prove withdrawal receipt missing")?;
                        let block_number = receipt
                            .block_number
                            .ok_or_eyre("prove receipt missing L1 block number")?;
                        let block = l1_provider
                            .get_block(BlockId::number(block_number))
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?
                            .ok_or_eyre("prove L1 block not found")?;
                        let proven_at = block.header.timestamp();
                        // recover the dispute game index from the prove calldata
                        let tx = l1_provider
                            .get_transaction_by_hash(*transaction_hash)
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?
                            .ok_or_eyre("prove transaction missing")?;
                        let call =
                            OptimismPortal::proveWithdrawalTransactionCall::abi_decode(tx.input())
                                .map_err(|err| StepError::Generic(err.into()))?;
                        let game_index: u64 = call
                            .disputeGameIndex
                            .try_into()
                            .map_err(|err: FromUintError<u64>| StepError::Generic(err.into()))?;
                        let factory = IDisputeGameFactoryInstance::new(
                            args.dispute_game_factory,
                            &l1_provider,
                        );
                        let entry = factory
                            .gameAtIndex(U256::from(game_index))
                            .call()
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?;
                        let game_addr = entry.proxy;
                        let game = IMultiProofGameInstance::new(game_addr, &l1_provider);
                        let game_l2_block: u64 = game
                            .l2SequenceNumber()
                            .call()
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?
                            .try_into()
                            .map_err(|err: FromUintError<u64>| StepError::Generic(err.into()))?;
                        let prove_withdrawal = ProveWithdrawal {
                            transaction: initiated_withdrawal.transaction,
                            hash: initiated_withdrawal.hash,
                            game_index,
                            game_l2_block,
                            game_addr,
                            proven_at,
                        };
                        write_json_with_backoff(
                            &args.proven,
                            &prove_withdrawal,
                            *transaction_hash,
                            StepStage::Prove,
                        )
                        .await?;
                        tokio::time::sleep(Duration::from_secs(12)).await;
                        continue;
                    }
                    _ => {
                        // other stages don't have the `PersistenceAfterTransaction` error variant
                    }
                }
            }

            // otherwise just exit the loop and return the error
            return Err(step_error);
        }
        tokio::time::sleep(Duration::from_secs(12)).await;
    }
}

/// Maximum write attempts (initial try + retries) when reconciling a handoff.
const MAX_PERSISTENCE_ATTEMPTS: u32 = 10;

/// Persist a handoff with bounded exponential backoff.
async fn write_json_with_backoff<T: Serialize>(
    location: &str,
    value: &T,
    transaction_hash: B256,
    stage: StepStage,
) -> Result<(), StepError> {
    let backoff = ExponentialBackoff::default();
    let mut attempts = 0u32;
    retry_notify(
        backoff,
        || {
            attempts += 1;
            let attempt = attempts;
            async move {
                match storage::write_json(location, value).await {
                    Ok(()) => Ok(()),
                    Err(err) if attempt >= MAX_PERSISTENCE_ATTEMPTS => {
                        Err(BackoffError::permanent(err))
                    }
                    Err(err) => Err(BackoffError::transient(err)),
                }
            }
        },
        |err, delay: Duration| {
            tracing::warn!(
                ?err,
                retry_in_secs = delay.as_secs(),
                %transaction_hash,
                %stage,
                "persistence after transaction failed; retrying"
            );
        },
    )
    .await
    .map_err(|err| StepError::PersistenceAfterTransaction {
        transaction_hash,
        stage,
        source: err,
    })
}
