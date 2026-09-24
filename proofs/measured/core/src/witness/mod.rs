pub mod preimage_store;

pub use kona_sp1_client_utils::witness::{BlobData, WitnessData};
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
mod tests;
