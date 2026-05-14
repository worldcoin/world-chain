use std::sync::{Arc, Mutex};

use alloy_consensus::Blob;
use alloy_eips::eip4844::env_settings::EnvKzgSettings;
use alloy_primitives::B256;
use anyhow::Result;
use async_trait::async_trait;
use kona_derive::BlobProvider;
use kona_protocol::BlockInfo;
use kzg_rs::{Blob as KzgRsBlob, Bytes48};
use world_chain_proof_succinct_client_utils::witness::BlobData;

#[derive(Clone, Debug)]
pub struct OnlineBlobStore<T: BlobProvider> {
    pub provider: T,
    pub store: Arc<Mutex<BlobData>>,
}

impl<T: BlobProvider + Send> OnlineBlobStore<T> {
    fn record_blob(&self, blob: &Blob) {
        let settings = EnvKzgSettings::default();
        let c_kzg_blob = c_kzg::Blob::from_bytes(blob.as_slice()).unwrap();
        let commitment = settings.get().blob_to_kzg_commitment(&c_kzg_blob).unwrap();
        let proof = settings
            .get()
            .compute_blob_kzg_proof(&c_kzg_blob, &commitment.to_bytes())
            .unwrap();

        let mut store = self.store.lock().unwrap();
        store
            .blobs
            .push(KzgRsBlob::from_slice(&*c_kzg_blob).unwrap());
        store.commitments.push(Bytes48(*commitment.to_bytes()));
        store.proofs.push(Bytes48(*proof.to_bytes()));
    }
}

#[async_trait]
impl<T: BlobProvider + Send> BlobProvider for OnlineBlobStore<T> {
    type Error = T::Error;

    async fn get_and_validate_blobs(
        &mut self,
        block_ref: &BlockInfo,
        blob_hashes: &[B256],
    ) -> Result<Vec<Box<Blob>>, Self::Error> {
        let blobs = self
            .provider
            .get_and_validate_blobs(block_ref, blob_hashes)
            .await?;
        for blob in &blobs {
            self.record_blob(blob);
        }
        Ok(blobs)
    }
}
