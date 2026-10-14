use std::{fmt::Debug, sync::Arc};

use alloy_consensus::{Header, Sealed};
use alloy_op_evm::{block::OpAlloyReceiptBuilder, post_exec::PostExecEvmFactoryAdapter};
use alloy_primitives::BlockNumber;
use anyhow::{Context, Result, ensure};
use kona_driver::{Executor, PipelineCursor};
use kona_preimage::CommsClient;
use kona_proof::{
    BootInfo, FlushableCache, executor::KonaExecutor, l1::OracleL1ChainProvider,
    l2::OracleL2ChainProvider,
};
use spin::RwLock;

use crate::precompiles::ZkvmOpEvmFactory;

/// Prepares the upstream pipeline and returns its starting L2 height for World public values.
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
    let (boot, inputs) =
        kona_sp1_client_utils::witness::executor::get_inputs_for_pipeline(oracle).await?;
    let safe_head_number = match &inputs {
        Some((cursor, _, l2_provider)) => {
            let safe_header = cursor.read().tip().l2_safe_head_header.clone();
            validate_starting_output_root(&boot, safe_header, l2_provider.clone())?;
            cursor.read().tip().l2_safe_head.block_info.number
        }
        // Upstream accepts a zero-step claim only when its height and root match the safe head.
        None => boot.claimed_l2_block_number,
    };
    Ok((boot, inputs, safe_head_number))
}

fn validate_starting_output_root<O>(
    boot: &BootInfo,
    safe_header: Sealed<Header>,
    l2_provider: OracleL2ChainProvider<O>,
) -> Result<()>
where
    O: CommsClient + Send + Sync + Debug,
{
    let mut executor = KonaExecutor::new(
        &boot.rollup_config,
        l2_provider.clone(),
        l2_provider,
        PostExecEvmFactoryAdapter::new(ZkvmOpEvmFactory::new()),
        OpAlloyReceiptBuilder::default(),
        None,
    );
    executor.update_safe_head(safe_header);
    // Authenticate the message-passer storage root against the starting state trie.
    let output_root = executor
        .compute_output_root()
        .context("failed to authenticate starting output root")?;
    ensure!(
        output_root == boot.agreed_l2_output_root,
        "Starting output root mismatch: agreed {}, computed {output_root}",
        boot.agreed_l2_output_root,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::Header;
    use alloy_primitives::{B256, Bytes, address, keccak256};
    use kona_preimage::{
        L1_HEAD_KEY, L2_CHAIN_ID_KEY, L2_CLAIM_BLOCK_NUMBER_KEY, L2_CLAIM_KEY, L2_OUTPUT_ROOT_KEY,
        L2_ROLLUP_CONFIG_KEY, PreimageKey,
    };
    use kona_proof::{block_on, sync::SyncStartError};
    use world_chain_proof_core::witness::preimage_store::PreimageStore;

    fn oracle(claimed_height: u64, matching_root: bool, version: u8) -> Arc<PreimageStore> {
        let header = Header {
            number: 42,
            ..Default::default()
        };
        let hash = header.hash_slow();
        let mut output = [0u8; 128];
        output[31] = version;
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
            block_on(get_inputs_for_pipeline(oracle(42, true, 0))).unwrap();
        assert!(inputs.is_none());
        assert_eq!(pre_height, 42);
        assert_eq!(boot.claimed_l2_block_number, pre_height);
        assert_eq!(boot.claimed_l2_output_root, boot.agreed_l2_output_root);
    }

    #[test]
    fn unchanged_height_rejects_different_root() {
        let error = block_on(get_inputs_for_pipeline(oracle(42, false, 0))).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SyncStartError>(),
            Some(SyncStartError::ClaimedRootMismatch { .. })
        ));
    }

    #[test]
    fn claim_before_safe_head_is_rejected() {
        let error = block_on(get_inputs_for_pipeline(oracle(41, true, 0))).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SyncStartError>(),
            Some(SyncStartError::ClaimedBlockBeforeSafeHead { .. })
        ));
    }

    #[test]
    fn pipeline_rejects_unsupported_output_version() {
        let error = block_on(get_inputs_for_pipeline(oracle(42, true, 1))).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SyncStartError>(),
            Some(SyncStartError::Oracle(
                kona_proof::errors::OracleProviderError::UnknownOutputVersion(_)
            ))
        ));
    }

    fn starting_root_fixture(
        state_root_override: Option<B256>,
        storage_root_override: Option<B256>,
        include_state_witness: bool,
    ) -> Arc<PreimageStore> {
        let storage_root = B256::repeat_byte(0x55);
        let account = alloy_rlp::encode(vec![
            Bytes::new(),
            Bytes::new(),
            Bytes::copy_from_slice(storage_root.as_slice()),
            Bytes::copy_from_slice(keccak256([]).as_slice()),
        ]);
        // A single-leaf state trie proves the message-passer account's storage root.
        let mut path = vec![0x20];
        path.extend_from_slice(
            keccak256(address!("4200000000000000000000000000000000000016")).as_slice(),
        );
        let state_node = alloy_rlp::encode(vec![Bytes::from(path), Bytes::from(account)]);
        let header = Header {
            number: 42,
            state_root: keccak256(&state_node),
            transactions_root: alloy_consensus::EMPTY_ROOT_HASH,
            ..Default::default()
        };
        let hash = header.hash_slow();
        let mut output_preimage = [0u8; 128];
        output_preimage[32..64]
            .copy_from_slice(state_root_override.unwrap_or(header.state_root).as_slice());
        output_preimage[64..96]
            .copy_from_slice(storage_root_override.unwrap_or(storage_root).as_slice());
        output_preimage[96..].copy_from_slice(hash.as_slice());
        let agreed_root = keccak256(output_preimage);
        let mut store = PreimageStore::default();
        // Even inconsistent output roots have valid Keccak preimages.
        store
            .save_preimage(
                PreimageKey::new_keccak256(agreed_root.0),
                output_preimage.to_vec(),
            )
            .unwrap();
        if include_state_witness {
            store
                .save_preimage(PreimageKey::new_keccak256(header.state_root.0), state_node)
                .unwrap();
        }
        let l1_header = Header::default();
        let l1_hash = l1_header.hash_slow();
        let chain_id = u64::MAX - 1;
        let mut config = kona_genesis::RollupConfig {
            l1_chain_id: 1,
            l2_chain_id: chain_id.into(),
            ..Default::default()
        };
        config.genesis.l1.hash = l1_hash;
        config.genesis.l2.hash = hash;
        config.genesis.l2.number = header.number;
        for (key, value) in [
            (
                PreimageKey::new_keccak256(hash.0),
                alloy_rlp::encode(&header),
            ),
            (
                PreimageKey::new_keccak256(l1_hash.0),
                alloy_rlp::encode(&l1_header),
            ),
            (
                PreimageKey::new_keccak256(alloy_consensus::EMPTY_ROOT_HASH.0),
                vec![0x80],
            ),
            (PreimageKey::new_local(L1_HEAD_KEY.to()), l1_hash.to_vec()),
            (
                PreimageKey::new_local(L2_OUTPUT_ROOT_KEY.to()),
                agreed_root.to_vec(),
            ),
            (
                PreimageKey::new_local(L2_CLAIM_KEY.to()),
                B256::ZERO.to_vec(),
            ),
            (
                PreimageKey::new_local(L2_CLAIM_BLOCK_NUMBER_KEY.to()),
                43u64.to_be_bytes().to_vec(),
            ),
            (
                PreimageKey::new_local(L2_CHAIN_ID_KEY.to()),
                chain_id.to_be_bytes().to_vec(),
            ),
            (
                PreimageKey::new_local(L2_ROLLUP_CONFIG_KEY.to()),
                serde_json::to_vec(&config).unwrap(),
            ),
        ] {
            store.save_preimage(key, value).unwrap();
        }
        Arc::new(store)
    }

    #[test]
    fn starting_output_root_accepts_authenticated_state_and_storage_roots() {
        let oracle = starting_root_fixture(None, None, true);
        let (boot, inputs, pre_height) = block_on(get_inputs_for_pipeline(oracle)).unwrap();
        assert!(inputs.is_some());
        assert_eq!(pre_height, 42);
        assert_eq!(boot.claimed_l2_block_number, 43);
    }

    #[test]
    fn starting_output_root_rejects_inconsistent_state_root() {
        let oracle = starting_root_fixture(Some(B256::repeat_byte(0x99)), None, true);
        let error = block_on(get_inputs_for_pipeline(oracle)).unwrap_err();
        assert!(
            error.to_string().contains("Starting output root mismatch"),
            "{error}"
        );
    }

    #[test]
    fn starting_output_root_rejects_inconsistent_message_passer_storage_root() {
        let oracle = starting_root_fixture(None, Some(B256::repeat_byte(0x99)), true);
        let error = block_on(get_inputs_for_pipeline(oracle)).unwrap_err();
        assert!(
            error.to_string().contains("Starting output root mismatch"),
            "{error}"
        );
    }

    #[test]
    fn starting_output_root_requires_message_passer_account_witness() {
        let oracle = starting_root_fixture(None, None, false);
        let error = block_on(get_inputs_for_pipeline(oracle)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to authenticate starting output root"),
            "{error}"
        );
    }
}
