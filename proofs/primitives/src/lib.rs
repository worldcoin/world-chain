//! World Chain proof-system primitives and contract bindings.
//!
//! WIP-1006 proof-domain primitives plus the stock OP Stack dispute interfaces
//! and the World Chain game extension used by the offchain services.

mod bindings;
mod consensus_provider;
mod types;

// re-exports
pub use bindings::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IWorldChainProofSystemGame,
};
pub use consensus_provider::{ConsensusError, ConsensusProvider, OptimismConsensusClient};
pub use types::{
    GameCreated, InvalidationReason, InvalidationReasonError, PROOF_LANE_COUNT,
    PROOF_SYSTEM_VERSION, PROOF_THRESHOLD, ProofDomain, ProofLane, ProposalCommitment,
    ResolutionStatus, RootCommitment, RootState, RootStateError, WIP_1006_GAME_TYPE, has_threshold,
    proof_count,
};
