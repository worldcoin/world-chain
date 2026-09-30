use std::{fmt::Debug, sync::Arc};

use alloy_op_evm::{block::OpAlloyReceiptBuilder, post_exec::PostExecEvmFactoryAdapter};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use kona_derive::{Pipeline, SignalReceiver};
use kona_driver::{Driver, DriverPipeline, PipelineCursor};
use kona_preimage::CommsClient;
use kona_proof::{BootInfo, FlushableCache, executor::KonaExecutor, l2::OracleL2ChainProvider};
use kona_sp1_client_utils::{
    metrics::CycleTrackerDriverMetrics, witness::executor::WitnessExecutor as KonaWitnessExecutor,
};
use spin::RwLock;
use tracing::info;

use world_chain_proof_core::range::WorldRangeHardforkConfig;

use crate::precompiles::{CustomCrypto, ZkvmOpEvmFactory};

/// World Chain extension of Kona's [`KonaWitnessExecutor`].
#[async_trait]
pub trait WitnessExecutor: KonaWitnessExecutor {
    async fn run_with_world_schedule<O, DP, P>(
        &self,
        boot: BootInfo,
        pipeline: DP,
        cursor: Arc<RwLock<PipelineCursor>>,
        l2_provider: OracleL2ChainProvider<O>,
        world_schedule: Option<WorldRangeHardforkConfig>,
    ) -> Result<BootInfo>
    where
        O: CommsClient + FlushableCache + Send + Sync + Debug,
        DP: DriverPipeline<P> + Send + Sync + Debug,
        P: Pipeline + SignalReceiver + Send + Sync + Debug,
    {
        revm::precompile::install_crypto(CustomCrypto::default());

        let boot_clone = boot.clone();
        let rollup_config = Arc::new(boot.rollup_config);

        let evm_factory = world_schedule.map_or_else(
            ZkvmOpEvmFactory::new,
            ZkvmOpEvmFactory::new_with_world_schedule,
        );

        let executor = KonaExecutor::new(
            rollup_config.as_ref(),
            l2_provider.clone(),
            l2_provider,
            PostExecEvmFactoryAdapter::new(evm_factory),
            OpAlloyReceiptBuilder::default(),
            None,
        );
        let mut driver = Driver::new(cursor, executor, pipeline);

        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-start: block-execution-and-derivation");
        let (safe_head, output_root) = driver
            .advance_to_target_with_metrics(
                rollup_config.as_ref(),
                Some(boot.claimed_l2_block_number),
                &CycleTrackerDriverMetrics,
            )
            .await?;
        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-end: block-execution-and-derivation");

        if output_root != boot.claimed_l2_output_root {
            return Err(anyhow!(
                "Failed to validate L2 block #{number} with claimed output root \
                 {claimed_output_root}. Got {output_root} instead",
                number = safe_head.block_info.number,
                output_root = output_root,
                claimed_output_root = boot.claimed_l2_output_root,
            ));
        }

        ensure_derived_block_matches_claim(
            safe_head.block_info.number,
            boot.claimed_l2_block_number,
        )?;

        info!(
            target: "client",
            "Successfully validated L2 block #{number} with output root {output_root}",
            number = safe_head.block_info.number,
            output_root = output_root
        );

        #[cfg(target_os = "zkvm")]
        {
            std::mem::forget(driver);
            std::mem::forget(rollup_config);
        }

        Ok(boot_clone)
    }
}

impl<T> WitnessExecutor for T where T: KonaWitnessExecutor + Send + Sync {}

fn ensure_derived_block_matches_claim(
    safe_head_number: u64,
    claimed_block_number: u64,
) -> Result<()> {
    if safe_head_number != claimed_block_number {
        return Err(anyhow!(
            "Derived safe head L2 block #{derived} does not match claimed L2 block \
             number #{claimed}",
            derived = safe_head_number,
            claimed = claimed_block_number,
        ));
    }
    Ok(())
}
