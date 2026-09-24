use std::{fmt::Debug, sync::Arc};

use alloy_primitives::BlockNumber;
use anyhow::{Result, anyhow};
use kona_driver::PipelineCursor;
use kona_preimage::CommsClient;
use kona_proof::{
    BootInfo, FlushableCache,
    l1::OracleL1ChainProvider,
    l2::OracleL2ChainProvider,
    sync::{DerivationInputs, prepare_derivation},
};
use spin::RwLock;

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

    let inputs = prepare_derivation(&boot, Arc::new(boot.rollup_config.clone()), oracle).await?;
    match inputs {
        DerivationInputs::TraceExtension => {
            // Kona has checked that both the claimed height and root equal the safe head.
            let safe_head_number = boot.claimed_l2_block_number;
            Ok((boot, None, safe_head_number))
        }
        DerivationInputs::Derive {
            cursor,
            l1_provider,
            l2_provider,
        } => {
            let safe_head_number = cursor.read().tip().l2_safe_head.block_info.number;
            Ok((
                boot,
                Some((cursor, l1_provider, l2_provider)),
                safe_head_number,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::Header;
    use alloy_primitives::{B256, keccak256};
    use kona_preimage::{
        L1_HEAD_KEY, L2_CHAIN_ID_KEY, L2_CLAIM_BLOCK_NUMBER_KEY, L2_CLAIM_KEY, L2_OUTPUT_ROOT_KEY,
        PreimageKey,
    };
    use kona_proof::{block_on, sync::SyncStartError};
    use world_chain_proof_core::witness::preimage_store::PreimageStore;

    fn oracle(claimed_height: u64, matching_root: bool) -> Arc<PreimageStore> {
        let header = Header {
            number: 42,
            ..Default::default()
        };
        let hash = header.hash_slow();
        let mut output = [0u8; 128];
        output[96..].copy_from_slice(hash.as_slice());
        let root = keccak256(output);
        let claim = if matching_root { root } else { B256::ZERO };
        let mut oracle = PreimageStore::default();
        for (key, value) in [
            (
                PreimageKey::new_keccak256(hash.0),
                alloy_rlp::encode(&header),
            ),
            (PreimageKey::new_keccak256(root.0), output.to_vec()),
            (
                PreimageKey::new_local(L1_HEAD_KEY.to()),
                B256::ZERO.to_vec(),
            ),
            (
                PreimageKey::new_local(L2_OUTPUT_ROOT_KEY.to()),
                root.to_vec(),
            ),
            (PreimageKey::new_local(L2_CLAIM_KEY.to()), claim.to_vec()),
            (
                PreimageKey::new_local(L2_CLAIM_BLOCK_NUMBER_KEY.to()),
                claimed_height.to_be_bytes().to_vec(),
            ),
            (
                PreimageKey::new_local(L2_CHAIN_ID_KEY.to()),
                10u64.to_be_bytes().to_vec(),
            ),
        ] {
            oracle.save_preimage(key, value).unwrap();
        }
        Arc::new(oracle)
    }

    #[test]
    fn unchanged_claim_preserves_starting_height_without_l1_witness() {
        let (boot, inputs, pre_height) =
            block_on(get_inputs_for_pipeline(oracle(42, true))).unwrap();
        assert!(inputs.is_none());
        assert_eq!(pre_height, 42);
        assert_eq!(boot.claimed_l2_block_number, pre_height);
        assert_eq!(boot.claimed_l2_output_root, boot.agreed_l2_output_root);
    }

    #[test]
    fn unchanged_height_rejects_different_root() {
        let error = block_on(get_inputs_for_pipeline(oracle(42, false))).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SyncStartError>(),
            Some(SyncStartError::ClaimedRootMismatch { .. })
        ));
    }

    #[test]
    fn claim_before_safe_head_is_rejected() {
        let error = block_on(get_inputs_for_pipeline(oracle(41, true))).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SyncStartError>(),
            Some(SyncStartError::ClaimedBlockBeforeSafeHead { .. })
        ));
    }
}
