//! World Chain proof-system primitives and contract bindings.
//!
//! This crate duplicates the WIP-1006-specific pieces that the World Chain
//! codebase needs directly: proof-domain hashing, root commitments, lane
//! bitmaps, and lightweight ABI bindings for the local proof contracts.

mod bindings;
mod output_root;
mod types;

// re-exports
pub use bindings::{
    IWorldChainAnchorStateRegistry, IWorldChainProofSystemFactory, IWorldChainProofSystemGame,
};
pub use output_root::{ConsensusError, ConsensusProvider, OptimismOutputRootClient};
pub use types::{
    PROOF_LANE_COUNT, PROOF_SYSTEM_VERSION, PROOF_THRESHOLD, ProofDomain, ProofLane,
    ProposalCommitment, RootCommitment, has_threshold, proof_count,
};
