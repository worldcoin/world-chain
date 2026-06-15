use std::{fmt::Debug, sync::Arc};

use alloy_op_evm::post_exec::PostExecEvmFactoryAdapter;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use kona_derive::{
    BlobProvider, ChainProvider, DataAvailabilityProvider, L2ChainProvider, Pipeline,
    SignalReceiver,
};
use kona_driver::{Driver, DriverPipeline, PipelineCursor};
use kona_genesis::{L1ChainConfig, RollupConfig};
use kona_preimage::CommsClient;
use kona_proof::{
    BootInfo, FlushableCache, executor::KonaExecutor, l1::OraclePipeline, l2::OracleL2ChainProvider,
};
use spin::RwLock;
use tracing::info;

use world_chain_proof_core::range::WorldRangeHardforkConfig;

use crate::{
    advance_to_target,
    precompiles::{CustomCrypto, ZkvmOpEvmFactory},
};

#[async_trait]
pub trait WitnessExecutor {
    type O: CommsClient + FlushableCache + Send + Sync + Debug;
    type B: BlobProvider + Send + Sync + Debug + Clone;
    type L1: ChainProvider + Send + Sync + Debug + Clone;
    type L2: L2ChainProvider + Send + Sync + Debug + Clone;
    type DA: DataAvailabilityProvider + Send + Sync + Debug + Clone;

    async fn create_pipeline(
        &self,
        rollup_config: Arc<RollupConfig>,
        l1_config: Arc<L1ChainConfig>,
        cursor: Arc<RwLock<PipelineCursor>>,
        oracle: Arc<Self::O>,
        beacon: Self::B,
        l1_provider: Self::L1,
        l2_provider: Self::L2,
    ) -> Result<OraclePipeline<Self::O, Self::L1, Self::L2, Self::DA>>;

    async fn run<O, DP, P>(
        &self,
        boot: BootInfo,
        pipeline: DP,
        cursor: Arc<RwLock<PipelineCursor>>,
        l2_provider: OracleL2ChainProvider<O>,
    ) -> Result<BootInfo>
    where
        O: CommsClient + FlushableCache + Send + Sync + Debug,
        DP: DriverPipeline<P> + Send + Sync + Debug,
        P: Pipeline + SignalReceiver + Send + Sync + Debug,
    {
        self.run_with_world_schedule(boot, pipeline, cursor, l2_provider, None)
            .await
    }

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
            None,
        );
        let mut driver = Driver::new(cursor, executor, pipeline);

        #[cfg(target_os = "zkvm")]
        println!("cycle-tracker-report-start: block-execution-and-derivation");
        let (safe_head, output_root) = advance_to_target(
            &mut driver,
            rollup_config.as_ref(),
            Some(boot.claimed_l2_block_number),
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

#[cfg(test)]
mod tests {
    use super::ensure_derived_block_matches_claim;

    #[test]
    fn returns_ok_when_derived_equals_claimed() {
        assert!(ensure_derived_block_matches_claim(0, 0).is_ok());
        assert!(ensure_derived_block_matches_claim(123_456, 123_456).is_ok());
        assert!(ensure_derived_block_matches_claim(u64::MAX, u64::MAX).is_ok());
    }

    #[test]
    fn returns_err_with_both_numbers_when_derived_below_claimed() {
        let err = ensure_derived_block_matches_claim(50, 100).expect_err("expected mismatch error");
        let msg = err.to_string();
        assert!(
            msg.contains("#50"),
            "missing derived block number in: {msg}"
        );
        assert!(
            msg.contains("#100"),
            "missing claimed block number in: {msg}"
        );
    }

    #[test]
    fn returns_err_with_both_numbers_when_derived_above_claimed() {
        let err =
            ensure_derived_block_matches_claim(150, 100).expect_err("expected mismatch error");
        let msg = err.to_string();
        assert!(msg.contains("#150"));
        assert!(msg.contains("#100"));
    }
}
