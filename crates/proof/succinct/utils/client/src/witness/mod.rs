// SP1-specific execution code.
pub mod executor;

// Re-export shared witness types from core so existing path-based imports keep working.
pub use world_chain_proof_core::witness::{
    BlobData, DefaultWitnessData, EigenDAWitnessData, WitnessData, WorldRangeWitnessData,
    preimage_store,
};
