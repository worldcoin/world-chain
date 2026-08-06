//! Host-side witness collection helpers for OP Succinct/Kona execution.

mod online_blob_store;
pub use online_blob_store::OnlineBlobStore;

mod preimage_witness_collector;
pub use preimage_witness_collector::PreimageWitnessCollector;
