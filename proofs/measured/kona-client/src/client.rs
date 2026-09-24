use alloy_primitives::B256;
use kona_derive::{Pipeline, SignalReceiver};
use kona_driver::{Driver, DriverPipeline, DriverResult, Executor};
use kona_genesis::RollupConfig;
pub use kona_proof::sync::fetch_safe_head_hash;
use kona_protocol::L2BlockInfo;
use kona_sp1_client_utils::metrics::CycleTrackerDriverMetrics;
use std::fmt::Debug;

/// Runs Kona derivation with SP1 cycle measurements.
#[allow(clippy::result_large_err)]
pub async fn advance_to_target<E, DP, P>(
    driver: &mut Driver<E, DP, P>,
    cfg: &RollupConfig,
    target: Option<u64>,
) -> DriverResult<(L2BlockInfo, B256), E::Error>
where
    E: Executor + Send + Sync + Debug,
    DP: DriverPipeline<P> + Send + Sync + Debug,
    P: Pipeline + SignalReceiver + Send + Sync + Debug,
{
    driver
        .advance_to_target_with_metrics(cfg, target, &CycleTrackerDriverMetrics)
        .await
}

#[cfg(test)]
mod tests {
    use super::fetch_safe_head_hash;
    use alloy_primitives::{B256, keccak256};
    use kona_preimage::PreimageKey;
    use kona_proof::{block_on, errors::OracleProviderError};
    use world_chain_proof_core::witness::preimage_store::PreimageStore;

    fn oracle_for_output(preimage: [u8; 128]) -> (PreimageStore, B256) {
        let root = keccak256(preimage);
        let mut oracle = PreimageStore::default();
        oracle
            .save_preimage(PreimageKey::new_keccak256(root.0), preimage.to_vec())
            .unwrap();
        (oracle, root)
    }

    #[test]
    fn version_zero_output_returns_safe_head() {
        let safe_head = B256::repeat_byte(0x42);
        let mut preimage = [0u8; 128];
        preimage[96..].copy_from_slice(safe_head.as_slice());
        let (oracle, root) = oracle_for_output(preimage);

        assert_eq!(
            block_on(fetch_safe_head_hash(&oracle, root)).unwrap(),
            safe_head
        );
    }

    #[test]
    fn nonzero_output_version_is_rejected() {
        for index in [0, 31] {
            let mut preimage = [0u8; 128];
            preimage[index] = 1;
            let version = B256::from_slice(&preimage[..32]);
            let (oracle, root) = oracle_for_output(preimage);

            let err = block_on(fetch_safe_head_hash(&oracle, root))
                .expect_err("unsupported output version must be rejected");
            assert!(matches!(err, OracleProviderError::UnknownOutputVersion(v) if v == version));
        }
    }
}
