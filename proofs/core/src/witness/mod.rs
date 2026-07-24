pub mod preimage_store;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use kzg_rs::{Blob, Bytes48};
use preimage_store::PreimageStore;
use serde::{Deserialize, Serialize};

use crate::{oracle::BlobStore, range::WorldRangeHardforkConfig};

#[async_trait]
pub trait WitnessData: Sized {
    /// Creates a new WitnessData from the given preimage store and blob data.
    fn from_parts(preimage_store: PreimageStore, blob_data: BlobData) -> Self;

    /// Consumes the WitnessData to extract its core components.
    fn into_parts(self) -> (PreimageStore, BlobData);

    /// Gets the oracle and blob provider from the witness data and validates the correctness of the
    /// preimages.
    async fn get_oracle_and_blob_provider(self) -> Result<(Arc<PreimageStore>, BlobStore)> {
        let (owned_preimage_store, owned_blob_data) = self.into_parts();

        println!("cycle-tracker-report-start: oracle-verify");
        owned_preimage_store
            .check_preimages()
            .expect("Failed to validate preimages");
        println!("cycle-tracker-report-end: oracle-verify");

        let oracle = Arc::new(owned_preimage_store);

        println!("cycle-tracker-report-start: blob-verification");
        let beacon = BlobStore::try_from(owned_blob_data)
            .map_err(|err| anyhow::anyhow!("failed to verify blob data: {err}"))?;
        println!("cycle-tracker-report-end: blob-verification");

        Ok((oracle, beacon))
    }
}

#[derive(Clone, Debug, Default, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct WorldRangeWitnessData {
    pub preimage_store: PreimageStore,
    pub blob_data: BlobData,
    pub schedule: WorldRangeHardforkConfig,
}

#[async_trait]
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

#[derive(
    Clone, Debug, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BlobData {
    pub blobs: Vec<Blob>,
    pub commitments: Vec<Bytes48>,
    pub proofs: Vec<Bytes48>,
}
