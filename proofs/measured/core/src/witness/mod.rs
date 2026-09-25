pub use kona_sp1_client_utils::witness::{BlobData, WitnessData, preimage_store};
use preimage_store::PreimageStore;

use crate::range::WorldRangeHardforkConfig;

#[derive(Clone, Debug, Default, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct WorldRangeWitnessData {
    pub preimage_store: PreimageStore,
    pub blob_data: BlobData,
    pub schedule: WorldRangeHardforkConfig,
}

impl WitnessData for WorldRangeWitnessData {
    fn from_parts(preimage_store: PreimageStore, blob_data: BlobData) -> Self {
        Self {
            preimage_store,
            blob_data,
            schedule: WorldRangeHardforkConfig::default(),
        }
    }

    fn into_parts(self) -> (PreimageStore, BlobData) {
        (self.preimage_store, self.blob_data)
    }
}

impl WorldRangeWitnessData {
    pub fn from_parts_with_world_config(
        preimage_store: PreimageStore,
        blob_data: BlobData,
        schedule: WorldRangeHardforkConfig,
    ) -> Self {
        Self {
            preimage_store,
            blob_data,
            schedule,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eips::eip4844::kzg_to_versioned_hash;
    use alloy_primitives::{hex_literal::hex, keccak256};
    use kona_derive::BlobProvider;
    use kona_preimage::{PreimageKey, PreimageOracleClient};
    use kona_proof::block_on;
    use kona_protocol::BlockInfo;
    use kzg_rs::{Blob, Bytes48};

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
    fn world_witness_roundtrip_preserves_schedule_and_preimages() {
        let value = b"world witness".to_vec();
        let key = PreimageKey::new_keccak256(keccak256(&value).0);
        let mut preimages = PreimageStore::default();
        preimages.save_preimage(key, value.clone()).unwrap();
        let schedule = WorldRangeHardforkConfig {
            tropo_time: Some(42),
            strato_time: Some(84),
            ..Default::default()
        };
        let witness = WorldRangeWitnessData::from_parts_with_world_config(
            preimages,
            BlobData::default(),
            schedule.clone(),
        );
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&witness).unwrap();
        let decoded =
            rkyv::from_bytes::<WorldRangeWitnessData, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(decoded.schedule, schedule);
        assert_eq!(block_on(decoded.preimage_store.get(key)).unwrap(), value);
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
    #[should_panic(expected = "KZG proof verification failed: invalid proofs")]
    fn upstream_witness_rejects_invalid_blob_proof() {
        let mut data = blob_data();
        data.blobs.swap(0, 1);
        let witness = WorldRangeWitnessData::from_parts(PreimageStore::default(), data);
        let _ = block_on(witness.get_oracle_and_blob_provider());
    }
}
