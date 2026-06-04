//! Shared utilities for World Chain range guest programs.

use std::sync::Arc;

use kona_proof::{l1::OracleL1ChainProvider, l2::OracleL2ChainProvider};
use world_chain_proof_core::{
    BlobStore,
    boot::{BootInfoStruct, RollupConfigHashError},
    range::WorldRangeHardforkConfig,
    witness::preimage_store::PreimageStore,
};
use world_chain_proof_succinct_client_utils::witness::executor::{
    WitnessExecutor, get_inputs_for_pipeline,
};

/// Sets up tracing for the range program.
#[cfg(feature = "tracing-subscriber")]
pub fn setup_tracing() {
    use anyhow::anyhow;
    use tracing::Level;

    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|err| anyhow!(err))
        .unwrap();
}

/// Runs the full Kona derivation + execution range program and returns public boot values.
pub async fn run_range_program<E>(
    executor: E,
    oracle: Arc<PreimageStore>,
    beacon: BlobStore,
    world_schedule: WorldRangeHardforkConfig,
) -> Result<BootInfoStruct, RollupConfigHashError>
where
    E: WitnessExecutor<
            O = PreimageStore,
            B = BlobStore,
            L1 = OracleL1ChainProvider<PreimageStore>,
            L2 = OracleL2ChainProvider<PreimageStore>,
        > + Send
        + Sync,
{
    let (boot_info, input) = get_inputs_for_pipeline(oracle.clone()).await.unwrap();
    let boot_info = match input {
        Some((cursor, l1_provider, l2_provider)) => {
            let rollup_config = Arc::new(boot_info.rollup_config.clone());
            let l1_config = Arc::new(boot_info.l1_config.clone());

            let pipeline = executor
                .create_pipeline(
                    rollup_config,
                    l1_config,
                    cursor.clone(),
                    oracle,
                    beacon,
                    l1_provider,
                    l2_provider.clone(),
                )
                .await
                .unwrap();

            executor
                .run_with_world_schedule(
                    boot_info,
                    pipeline,
                    cursor,
                    l2_provider,
                    Some(world_schedule.clone()),
                )
                .await
                .unwrap()
        }
        None => boot_info,
    };

    BootInfoStruct::try_from_kona_boot_info(boot_info, &world_schedule)
}
