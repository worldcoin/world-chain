//! SP1 program that aggregates World Chain range proofs.

#![cfg_attr(target_os = "zkvm", no_main)]

#[cfg(target_os = "zkvm")]
sp1_zkvm::entrypoint!(main);

use std::collections::HashMap;

use alloy_consensus::Header;
use alloy_primitives::B256;
use alloy_sol_types::SolValue;
use sha2::{Digest, Sha256};
use world_chain_proof_core::{
    boot::TransitionPublicValues,
    types::{AggregationInputs, AggregationOutputs, u32_to_u8},
};

pub fn main() {
    let agg_inputs = sp1_zkvm::io::read::<AggregationInputs>();
    let headers_bytes = sp1_zkvm::io::read_vec();
    let headers: Vec<Header> = serde_cbor::from_slice(&headers_bytes).unwrap();
    assert!(!agg_inputs.boot_infos.is_empty());

    agg_inputs.boot_infos.windows(2).for_each(|pair| {
        let (prev_boot_info, boot_info) = (&pair[0], &pair[1]);
        assert_eq!(prev_boot_info.l2PostRoot, boot_info.l2PreRoot);
        assert_eq!(prev_boot_info.rollupConfigHash, boot_info.rollupConfigHash);
    });

    agg_inputs.boot_infos.iter().for_each(|boot_info| {
        let serialized_boot_info = bincode::serialize(boot_info).unwrap();
        let pv_digest = Sha256::digest(serialized_boot_info);

        sp1_lib::verify::verify_sp1_proof(&agg_inputs.multi_block_vkey, &pv_digest.into());
    });

    let mut l1_heads_map: HashMap<B256, bool> = agg_inputs
        .boot_infos
        .iter()
        .map(|boot_info| (boot_info.l1Head, false))
        .collect();

    let mut current_hash = agg_inputs.latest_l1_checkpoint_head;
    for header in headers.iter().rev() {
        assert_eq!(current_hash, header.hash_slow());

        if let Some(found) = l1_heads_map.get_mut(&current_hash) {
            *found = true;
        }

        current_hash = header.parent_hash;
    }

    for (l1_head, found) in l1_heads_map.iter() {
        assert!(
            *found,
            "l1 head {l1_head:?} not found in the provided header chain"
        );
    }

    let first_boot_info = &agg_inputs.boot_infos[0];
    let last_boot_info = &agg_inputs.boot_infos[agg_inputs.boot_infos.len() - 1];
    let final_boot_info = TransitionPublicValues {
        l1Head: agg_inputs.latest_l1_checkpoint_head,
        l2PreRoot: first_boot_info.l2PreRoot,
        l2PostRoot: last_boot_info.l2PostRoot,
        l2BlockNumber: last_boot_info.l2BlockNumber,
        rollupConfigHash: last_boot_info.rollupConfigHash,
    };

    let multi_block_vkey_b256 = B256::from(u32_to_u8(agg_inputs.multi_block_vkey));
    let agg_outputs = AggregationOutputs {
        l1Head: final_boot_info.l1Head,
        l2PreRoot: final_boot_info.l2PreRoot,
        l2PostRoot: final_boot_info.l2PostRoot,
        l2BlockNumber: final_boot_info.l2BlockNumber,
        rollupConfigHash: final_boot_info.rollupConfigHash,
        multiBlockVKey: multi_block_vkey_b256,
        proverAddress: agg_inputs.prover_address,
    };

    sp1_zkvm::io::commit_slice(&agg_outputs.abi_encode());
}
