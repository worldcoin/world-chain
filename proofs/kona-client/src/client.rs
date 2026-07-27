use alloy_consensus::BlockBody;
use alloy_primitives::B256;
use alloy_rlp::Decodable;
use anyhow::Result;
use kona_derive::{Pipeline, PipelineError, PipelineErrorKind, Signal, SignalReceiver};
use kona_driver::{Driver, DriverError, DriverPipeline, DriverResult, Executor, TipCursor};
use kona_genesis::RollupConfig;
use kona_preimage::{CommsClient, PreimageKey};
use kona_proof::{HintType, errors::OracleProviderError};
use kona_protocol::L2BlockInfo;
use op_alloy_consensus::{OpBlock, OpTxEnvelope, OpTxType};
use std::fmt::Debug;
use tracing::{error, info, warn};

/// Fetches the safe head hash of the L2 chain based on the agreed upon L2 output root.
pub async fn fetch_safe_head_hash<O>(
    caching_oracle: &O,
    agreed_l2_output_root: B256,
) -> Result<B256, OracleProviderError>
where
    O: CommsClient,
{
    let mut output_preimage = [0u8; 128];
    HintType::StartingL2Output
        .with_data(&[agreed_l2_output_root.as_ref()])
        .send(caching_oracle)
        .await?;
    caching_oracle
        .get_exact(
            PreimageKey::new_keccak256(*agreed_l2_output_root),
            output_preimage.as_mut(),
        )
        .await?;

    output_preimage[96..128]
        .try_into()
        .map_err(OracleProviderError::SliceConversion)
}

/// Advances the derivation pipeline to the target block number.
#[allow(clippy::result_large_err)]
pub async fn advance_to_target<E, DP, P>(
    driver: &mut Driver<E, DP, P>,
    cfg: &RollupConfig,
    mut target: Option<u64>,
) -> DriverResult<(L2BlockInfo, B256), E::Error>
where
    E: Executor + Send + Sync + Debug,
    DP: DriverPipeline<P> + Send + Sync + Debug,
    P: Pipeline + SignalReceiver + Send + Sync + Debug,
{
    let start_block_number = driver.cursor.read().tip().l2_safe_head.block_info.number;
    // Progress is logged at least every 10% of the range, but never less often than every
    // 100 blocks, so small ranges still get a handful of updates and huge ranges don't get
    // spammed. `target` is `None` for open-ended derivation (no fixed range), in which case
    // we just fall back to a flat every-100-blocks cadence.
    let progress_interval = target
        .map(|target_block| {
            target_block
                .saturating_sub(start_block_number)
                .max(1)
                .div_ceil(10)
                .min(100)
        })
        .unwrap_or(100)
        .max(1);
    let mut last_logged_block_number = start_block_number;

    loop {
        let pipeline_cursor = driver.cursor.read();
        let tip_cursor = pipeline_cursor.tip();
        if let Some(tb) = target
            && tip_cursor.l2_safe_head.block_info.number >= tb
        {
            info!(target: "client", "Derivation complete, reached L2 safe head.");
            return Ok((tip_cursor.l2_safe_head, tip_cursor.l2_safe_head_output_root));
        }

        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-start: payload-derivation");
        let mut attributes = match driver
            .pipeline
            .produce_payload(tip_cursor.l2_safe_head)
            .await
        {
            Ok(attrs) => attrs.take_inner(),
            Err(PipelineErrorKind::Critical(PipelineError::EndOfSource)) => {
                warn!(target: "client", "Exhausted data source; Halting derivation and using current safe head.");

                if target.is_some() {
                    target = Some(tip_cursor.l2_safe_head.block_info.number);
                };

                if cfg.is_interop_active(driver.cursor.read().l2_safe_head().block_info.timestamp) {
                    return Err(PipelineError::EndOfSource.crit().into());
                } else {
                    continue;
                }
            }
            Err(e) => {
                error!(target: "client", "Failed to produce payload: {:?}", e);
                return Err(DriverError::Pipeline(e));
            }
        };
        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-end: payload-derivation");

        driver
            .executor
            .update_safe_head(tip_cursor.l2_safe_head_header.clone());

        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-start: block-execution");
        let outcome = match driver.executor.execute_payload(attributes.clone()).await {
            Ok(outcome) => outcome,
            Err(e) => {
                error!(target: "client", "Failed to execute L2 block: {}", e);

                if cfg.is_holocene_active(attributes.payload_attributes.timestamp) {
                    warn!(target: "client", "Flushing current channel and retrying deposit only block");

                    driver.pipeline.signal(Signal::FlushChannel).await?;

                    attributes.transactions = attributes.transactions.map(|txs| {
                        txs.into_iter()
                            .filter(|tx| !tx.is_empty() && tx[0] == OpTxType::Deposit as u8)
                            .collect::<Vec<_>>()
                    });

                    driver
                        .executor
                        .update_safe_head(tip_cursor.l2_safe_head_header.clone());
                    match driver.executor.execute_payload(attributes.clone()).await {
                        Ok(header) => header,
                        Err(e) => {
                            error!(
                                target: "client",
                                "Critical - Failed to execute deposit-only block: {e}",
                            );
                            return Err(DriverError::Executor(e));
                        }
                    }
                } else {
                    continue;
                }
            }
        };
        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-end: block-execution");

        let block = OpBlock {
            header: outcome.header.inner().clone(),
            body: BlockBody {
                transactions: attributes
                    .transactions
                    .as_ref()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .map(|tx| OpTxEnvelope::decode(&mut tx.as_ref()).map_err(DriverError::Rlp))
                    .collect::<DriverResult<Vec<OpTxEnvelope>, E::Error>>()?,
                ommers: Vec::new(),
                withdrawals: None,
            },
        };

        let origin = driver
            .pipeline
            .origin()
            .ok_or(PipelineError::MissingOrigin.crit())?;
        let l2_info =
            L2BlockInfo::from_block_and_genesis(&block, &driver.pipeline.rollup_config().genesis)?;
        let tip_cursor = TipCursor::new(
            l2_info,
            outcome.header,
            driver
                .executor
                .compute_output_root()
                .map_err(DriverError::Executor)?,
        );

        drop(pipeline_cursor);
        driver.cursor.write().advance(origin, tip_cursor);

        let processed_block_number = l2_info.block_info.number;
        if processed_block_number.saturating_sub(last_logged_block_number) >= progress_interval {
            last_logged_block_number = processed_block_number;
            match target {
                Some(target_block) => {
                    let total = target_block.saturating_sub(start_block_number).max(1);
                    let done = processed_block_number.saturating_sub(start_block_number);
                    let percent = done.saturating_mul(100) / total;
                    info!(
                        target: "client",
                        "Processed block {processed_block_number} ({done}/{total}, {percent}%)"
                    );
                }
                None => {
                    info!(target: "client", "Processed block {processed_block_number}");
                }
            }
        }

        #[cfg(target_os = "zkvm")]
        std::mem::forget(block);
    }
}
