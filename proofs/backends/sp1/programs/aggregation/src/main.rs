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
    types::{AggregationInputs, AggregationPublicValues, u32_to_u8},
};

pub fn main() {
    let agg_inputs = sp1_zkvm::io::read::<AggregationInputs>();
    let headers_bytes = sp1_zkvm::io::read_vec();
    let headers: Vec<Header> = serde_cbor::from_slice(&headers_bytes).unwrap();
    assert!(!agg_inputs.transition_public_values.is_empty());

    agg_inputs
        .transition_public_values
        .windows(2)
        .for_each(|pair| {
            let (previous, current) = (&pair[0], &pair[1]);
            assert_eq!(previous.l2PostRoot, current.l2PreRoot);
            assert_eq!(previous.l2PostBlockNumber, current.l2PreBlockNumber);
            assert_eq!(previous.rollupConfigHash, current.rollupConfigHash);
        });

    agg_inputs
        .transition_public_values
        .iter()
        .for_each(|transition_public_values| {
            let serialized_public_values = bincode::serialize(transition_public_values).unwrap();
            let pv_digest = Sha256::digest(serialized_public_values);

            sp1_lib::verify::verify_sp1_proof(&agg_inputs.multi_block_vkey, &pv_digest.into());
        });

    let mut l1_heads_map: HashMap<B256, bool> = agg_inputs
        .transition_public_values
        .iter()
        .map(|transition_public_values| (transition_public_values.l1Head, false))
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

    let first = &agg_inputs.transition_public_values[0];
    let last =
        &agg_inputs.transition_public_values[agg_inputs.transition_public_values.len() - 1];
    let aggregated_transition_public_values = TransitionPublicValues {
        l1Head: agg_inputs.latest_l1_checkpoint_head,
        l2PreRoot: first.l2PreRoot,
        l2PreBlockNumber: first.l2PreBlockNumber,
        l2PostRoot: last.l2PostRoot,
        l2PostBlockNumber: last.l2PostBlockNumber,
        rollupConfigHash: last.rollupConfigHash,
    };

    let multi_block_vkey_b256 = B256::from(u32_to_u8(agg_inputs.multi_block_vkey));
    let aggregation_public_values = AggregationPublicValues {
        transitionPublicValues: aggregated_transition_public_values,
        multiBlockVKey: multi_block_vkey_b256,
    };

    sp1_zkvm::io::commit_slice(&aggregation_public_values.abi_encode());
}
