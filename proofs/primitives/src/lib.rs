//! World Chain proof-system primitives and contract bindings.
//!
//! This crate duplicates the WIP-1006-specific pieces that the World Chain
//! codebase needs directly: proof-domain hashing, root commitments, lane
//! bitmaps, and lightweight ABI bindings for the local proof contracts.

mod bindings;
mod consensus_provider;
mod types;

// re-exports
pub use bindings::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IWorldChainProofSystemGame,
};
pub use consensus_provider::{ConsensusError, ConsensusProvider, OptimismConsensusClient};
pub use types::{
    ANCHOR_PARENT_INDEX, GameCreated, GameStatus, GameStatusError, InvalidationReason,
    InvalidationReasonError, PROOF_LANE_COUNT, PROOF_SYSTEM_VERSION, PROOF_THRESHOLD, ProofDomain,
    ProofLane, ProposalCommitment, ResolutionStatus, RootCommitment, RootState, RootStateError,
    extra_data, has_threshold, proof_count,
};
