use std::{fmt::Debug, sync::Arc};

use alloy_primitives::{BlockNumber, Sealed};
use anyhow::{Result, anyhow};
use kona_driver::PipelineCursor;
use kona_executor::TrieDBProvider as _;
use kona_preimage::CommsClient;
use kona_proof::{
    BootInfo, FlushableCache, l1::OracleL1ChainProvider, l2::OracleL2ChainProvider,
    sync::new_oracle_pipeline_cursor,
};
use spin::RwLock;

use crate::client::fetch_safe_head_hash;

/// Loads boot info and constructs the initial pipeline cursor and providers.
pub async fn get_inputs_for_pipeline<O>(
    oracle: Arc<O>,
) -> Result<(
    BootInfo,
    Option<(
        Arc<RwLock<PipelineCursor>>,
        OracleL1ChainProvider<O>,
        OracleL2ChainProvider<O>,
    )>,
    BlockNumber,
)>
where
    O: CommsClient + FlushableCache + Send + Sync + Debug,
{
    let boot = match BootInfo::load(oracle.as_ref()).await {
        Ok(boot) => boot,
        Err(e) => {
            return Err(anyhow!("Failed to load boot info: {:?}", e));
        }
    };

    let boot_clone = boot.clone();

    let rollup_config = Arc::new(boot.rollup_config);
    let safe_head_hash = fetch_safe_head_hash(oracle.as_ref(), boot.agreed_l2_output_root).await?;

    let mut l1_provider = OracleL1ChainProvider::new(boot.l1_head, oracle.clone());
    let mut l2_provider =
        OracleL2ChainProvider::new(safe_head_hash, rollup_config.clone(), oracle.clone());

    let safe_head = l2_provider
        .header_by_hash(safe_head_hash)
        .map(|header| Sealed::new_unchecked(header, safe_head_hash))?;
    let safe_head_block_number = safe_head.number;

    if boot.claimed_l2_block_number < safe_head.number {
        return Err(anyhow!(
            "Claimed L2 block number {claimed} is less than the safe head {safe}",
            claimed = boot.claimed_l2_block_number,
            safe = safe_head.number
        ));
    }

    let cursor = new_oracle_pipeline_cursor(
        rollup_config.as_ref(),
        safe_head,
        boot.agreed_l2_output_root,
        &mut l1_provider,
        &mut l2_provider,
    )
    .await?;
    l2_provider.set_cursor(cursor.clone());

    Ok((
        boot_clone,
        Some((cursor, l1_provider, l2_provider)),
        safe_head_block_number,
    ))
}
