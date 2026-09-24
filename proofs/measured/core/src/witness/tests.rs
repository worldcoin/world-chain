use super::*;
use alloy_eips::eip4844::kzg_to_versioned_hash;
use alloy_primitives::{B256, hex_literal::hex, keccak256};
use kona_derive::BlobProvider;
use kona_preimage::{PreimageKey, PreimageOracleClient};
use kona_proof::block_on;
use kona_protocol::BlockInfo;
use kzg_rs::{Blob, Bytes48};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Freeze the pre-refactor wire shapes to test both directions across the upstream type boundary.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Serialize, Deserialize)]
struct LegacyPreimageStore {
    preimage_map: HashMap<PreimageKey, Vec<u8>>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Serialize, Deserialize)]
struct LegacyBlobData {
    blobs: Vec<Blob>,
    commitments: Vec<Bytes48>,
    proofs: Vec<Bytes48>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct LegacyWorldWitness {
    preimage_store: LegacyPreimageStore,
    blob_data: LegacyBlobData,
    schedule: WorldRangeHardforkConfig,
}

fn blob_data() -> BlobData {
    // Constant-zero and constant-two vectors from kzg-rs 0.2.8's verify_blob_kzg_proof tests.
    let mut infinity = [0; 48];
    infinity[0] = 0xc0;
    let two_commitment = hex!(
        "a572cbea904d67468808c8eb50a9450c9721db309128012543902d0ac358a62ae28f75bb8f1c7c42c39a8c5529bf0f4e"
    );
    let mut twos = vec![0; 131072];
    for field in twos.as_chunks_mut::<32>().0 {
        field[31] = 2;
    }
    BlobData {
        blobs: vec![
            Blob::from_slice(&vec![0; 131072]).unwrap(),
            Blob::from_slice(&twos).unwrap(),
        ],
        commitments: vec![Bytes48(infinity), Bytes48(two_commitment)],
        proofs: vec![Bytes48(infinity); 2],
    }
}

#[test]
fn legacy_and_upstream_witnesses_deserialize_in_both_directions() {
    let value = b"witness compatibility".to_vec();
    let key = PreimageKey::new_keccak256(keccak256(&value).0);
    let blobs = blob_data();
    let old = LegacyWorldWitness {
        preimage_store: LegacyPreimageStore {
            preimage_map: HashMap::from([(key, value.clone())]),
        },
        blob_data: LegacyBlobData {
            blobs: blobs.blobs,
            commitments: blobs.commitments,
            proofs: blobs.proofs,
        },
        schedule: WorldRangeHardforkConfig {
            tropo_time: Some(42),
            strato_time: Some(84),
            ..Default::default()
        },
    };
    let old_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&old).unwrap();
    let new = rkyv::from_bytes::<WorldRangeWitnessData, rkyv::rancor::Error>(&old_bytes).unwrap();
    new.preimage_store.check_preimages().unwrap();
    assert_eq!(block_on(new.preimage_store.get(key)).unwrap(), value);
    assert_eq!(
        bincode::serialize(&old.preimage_store).unwrap(),
        bincode::serialize(&new.preimage_store).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&old.blob_data).unwrap(),
        serde_json::to_value(&new.blob_data).unwrap()
    );
    assert_eq!(new.schedule, old.schedule);
    let new_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&new).unwrap();
    let roundtrip =
        rkyv::from_bytes::<LegacyWorldWitness, rkyv::rancor::Error>(&new_bytes).unwrap();
    assert_eq!(
        roundtrip.preimage_store.preimage_map,
        old.preimage_store.preimage_map
    );
    assert_eq!(
        serde_json::to_value(roundtrip.blob_data).unwrap(),
        serde_json::to_value(old.blob_data).unwrap()
    );
    assert_eq!(roundtrip.schedule, old.schedule);
}

#[test]
fn upstream_witness_returns_verified_blobs_in_requested_order() {
    let data = blob_data();
    let hashes: Vec<_> = data
        .commitments
        .iter()
        .map(|c| kzg_to_versioned_hash(c.as_slice()))
        .collect();
    let expected_first = data.blobs[1].0.to_vec();
    let expected_second = data.blobs[0].0.to_vec();
    let witness = WorldRangeWitnessData::from_parts(PreimageStore::default(), data);
    let (_, mut store) = block_on(witness.get_oracle_and_blob_provider()).unwrap();
    let result =
        block_on(store.get_and_validate_blobs(&BlockInfo::default(), &[hashes[1], hashes[0]]))
            .unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].as_slice(), expected_first);
    assert_eq!(result[1].as_slice(), expected_second);
}

#[test]
#[should_panic]
fn upstream_witness_rejects_empty_blob_count_mismatch() {
    let data = BlobData {
        commitments: vec![Bytes48([0; 48])],
        ..Default::default()
    };
    let witness = WorldRangeWitnessData::from_parts(PreimageStore::default(), data);
    let _ = block_on(witness.get_oracle_and_blob_provider());
}

#[test]
#[should_panic]
fn upstream_witness_rejects_invalid_blob_proof() {
    let mut data = blob_data();
    data.blobs.swap(0, 1);
    let witness = WorldRangeWitnessData::from_parts(PreimageStore::default(), data);
    let _ = block_on(witness.get_oracle_and_blob_provider());
}

#[test]
#[should_panic(expected = "requested blob hash not present")]
fn upstream_store_rejects_missing_blob() {
    let mut store = crate::BlobStore::default();
    let _ = block_on(store.get_and_validate_blobs(&BlockInfo::default(), &[B256::ZERO]));
}

#[test]
fn upstream_store_rejects_invalid_preimages_and_conflicting_local_values() {
    let mut store = PreimageStore::default();
    let key = PreimageKey::new_keccak256(keccak256(b"valid").0);
    assert!(store.save_preimage(key, b"wrong".to_vec()).is_err());
    store.save_preimage(key, b"valid".to_vec()).unwrap();
    let local = PreimageKey::new_local(1);
    store.save_preimage(local, b"first".to_vec()).unwrap();
    assert!(store.save_preimage(local, b"second".to_vec()).is_err());
    assert_eq!(block_on(store.get(local)).unwrap(), b"first");
    store.check_preimages().unwrap();
}
