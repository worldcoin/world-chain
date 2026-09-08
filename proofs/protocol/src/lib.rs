//! World Chain proof-system primitives and contract bindings.
//!
//! This crate duplicates the WIP-1006-specific pieces that the World Chain
//! codebase needs directly: proof-domain hashing, root commitments, lane
//! bitmaps, and lightweight ABI bindings for the local proof contracts.

/// Default timeout for confirming an L1 transaction receipt.
pub const DEFAULT_L1_TX_RECEIPT_TIMEOUT_SECONDS: u64 = 5 * 60;

mod bindings;
mod consensus_provider;
mod lineage;
mod proof_game;
mod types;

// re-exports
pub use bindings::{
    IAnchorStateRegistry, IDisputeGameFactory, IERC20StakingVault, IMultiProofGame,
};
pub use consensus_provider::{
    ConsensusError, ConsensusProvider, OptimismConsensusClient, VerifyingConsensusProvider,
};
pub use lineage::{
    LineageAnchor, LineageError, LineageGame, LineageProvider, LineageStop, LineageTransition,
    RegisteredLineageConfig, SelectedLineage, SelectedLineageGame, read_game_for_transition,
    read_game_has_retry, read_lineage_anchor, read_lineage_resolution_status,
    read_registered_bond_vault, read_registered_lineage_config, select_lineage,
};
pub use proof_game::{
    AlloyProofGameProvider, ProofGameContext, ProofGameContextError, ProofGameProvider,
};
pub use types::{
    ClaimData, GameCreation, GameStatus, GameStatusError, InvalidationReason,
    InvalidationReasonError, MAX_ATTEMPT_SCAN, MULTI_PROOF_GAME_TYPE, PROOF_HEADER_LENGTH,
    PROOF_LANE_COUNT, PROOF_SYSTEM_VERSION, PROOF_THRESHOLD, ProofDomain, ProofLane,
    ProposalCommitment, ProposalStatus, ProposalStatusError, ResolutionStatus, RootCommitment,
    encode_compact_proof, has_threshold, proof_count,
};
