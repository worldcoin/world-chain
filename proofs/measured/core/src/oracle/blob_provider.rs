use alloc::{boxed::Box, vec::Vec};
use alloy_consensus::Blob;
use alloy_eips::eip4844::kzg_to_versioned_hash;
use alloy_primitives::B256;
use async_trait::async_trait;
use kona_derive::{BlobProvider, BlobProviderError};
use kona_protocol::BlockInfo;
use kzg_rs::{KzgError, get_kzg_settings};

use crate::witness::BlobData;

#[derive(Clone, Debug, Default)]
pub struct BlobStore {
    versioned_blobs: Vec<(B256, Blob)>,
}

/// Errors that can occur while converting raw [`BlobData`] into a verified [`BlobStore`].
///
/// The conversion is fallible because both the blob payload bytes and the KZG
/// proofs/commitments come from the preimage oracle (which in turn was populated from the L1
/// beacon chain). Malformed or tampered data must surface as a `Result` rather than a panic
/// because callers run inside the guest program and we never want network-controlled input to
/// abort the zkVM.
#[derive(Debug, thiserror::Error)]
pub enum BlobStoreError {
    /// One of the raw blob byte slices was not 4096 field elements wide.
    #[error("invalid blob bytes at index {index}: {error:?}")]
    InvalidBlob {
        /// Position of the offending blob in [`BlobData::blobs`].
        index: usize,
        /// Underlying KZG error returned by `kzg_rs`. `kzg_rs::KzgError` does not implement
        /// `std::error::Error`, so it is intentionally not annotated with `#[source]`.
        error: KzgError,
    },
    /// KZG batch verification reported the proofs as invalid.
    #[error("kzg batch proof verification failed: proofs are invalid")]
    InvalidProofs,
    /// KZG batch verification returned an internal error.
    #[error("kzg batch proof verification error: {0:?}")]
    KzgError(KzgError),
}

impl TryFrom<BlobData> for BlobStore {
    type Error = BlobStoreError;

    fn try_from(value: BlobData) -> Result<Self, Self::Error> {
        let blobs: Vec<kzg_rs::Blob> = value
            .blobs
            .iter()
            .enumerate()
            .map(|(index, b)| {
                kzg_rs::Blob::from_slice(&b.0)
                    .map_err(|error| BlobStoreError::InvalidBlob { index, error })
            })
            .collect::<Result<_, _>>()?;
        let versioned_blobs = value
            .commitments
            .iter()
            .map(|c| kzg_to_versioned_hash(c.as_slice()))
            .zip(blobs.iter().map(|b| Blob::from(b.0)))
            .rev()
            .collect();

        match kzg_rs::KzgProof::verify_blob_kzg_proof_batch(
            blobs,
            value.commitments,
            value.proofs,
            &get_kzg_settings(),
        ) {
            Ok(true) => {}
            Ok(false) => return Err(BlobStoreError::InvalidProofs),
            Err(e) => return Err(BlobStoreError::KzgError(e)),
        }

        Ok(Self { versioned_blobs })
    }
}

#[async_trait]
impl BlobProvider for BlobStore {
    type Error = BlobProviderError;

    async fn get_and_validate_blobs(
        &mut self,
        _: &BlockInfo,
        blob_hashes: &[B256],
    ) -> Result<Vec<Box<Blob>>, Self::Error> {
        Ok(blob_hashes
            .iter()
            .filter_map(|hash| {
                let (blob_hash, blob) = self.versioned_blobs.pop().unwrap();
                (*hash == blob_hash).then(|| Box::new(blob))
            })
            .collect())
    }
}
