use alloy_primitives::{Address, B256, BlockNumber, Bytes, U256, keccak256};
use alloy_sol_types::SolValue;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Number of distinct lanes required by WIP-1006 for challenged finality.
pub const PROOF_THRESHOLD: u8 = 2;

/// Number of configured proof lanes.
pub const PROOF_LANE_COUNT: u8 = 3;

/// Version of the World Chain proof-domain encoding implemented here.
pub const PROOF_SYSTEM_VERSION: u64 = 1;

/// OP Stack dispute-game type allocated to WIP-1006 (`GameTypes.MULTI_PROOF_GAME_TYPE`).
///
/// The stock `DisputeGameFactory` indexes every game type in one array, so every
/// index-based read must filter on this value.
pub const MULTI_PROOF_GAME_TYPE: u32 = 1006;

/// Maximum number of sequential retry attempts probed for one transition.
pub const MAX_ATTEMPT_SCAN: u64 = 64;

/// Byte length of the `submitProofLane` compact header.
///
/// Must match `LibProof.PROOF_HEADER_LENGTH`.
pub const PROOF_HEADER_LENGTH: usize = 21;

/// Proposal context fixed when a `MultiProofGame` is created.
#[derive(Debug, Clone, Copy)]
pub struct GameCreation {
    pub root_id: B256,
    pub game: Address,
    pub game_creator: Address,
    pub root_claim: B256,
    pub l2_block_number: BlockNumber,
    pub parent_ref: Address,
    pub l1_origin_hash: B256,
    pub l1_origin_number: BlockNumber,
    pub attempt: u64,
}

/// Stock OP Stack `GameStatus`, as returned by `status()` and `resolutionStatus().outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GameStatus {
    InProgress = 0,
    /// The root claim was successfully challenged.
    ChallengerWins = 1,
    /// The root claim could not be contested.
    DefenderWins = 2,
}

#[derive(Debug, Error)]
pub enum GameStatusError {
    #[error("Invalid game status: {0}")]
    InvalidGameStatus(u8),
}

impl TryFrom<u8> for GameStatus {
    type Error = GameStatusError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::InProgress),
            1 => Ok(Self::ChallengerWins),
            2 => Ok(Self::DefenderWins),
            _ => Err(GameStatusError::InvalidGameStatus(value)),
        }
    }
}

/// `IMultiProofGame.ProposalStatus`, the per-proposal state machine behind `claimData`.
///
/// Distinct from [`GameStatus`]: it splits an in-progress game into challenged and unchallenged,
/// which the game's `GameStatus` cannot express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProposalStatus {
    Unchallenged = 0,
    Challenged = 1,
    UnchallengedAndValidProofProvided = 2,
    ChallengedAndValidProofProvided = 3,
    Resolved = 4,
}

impl ProposalStatus {
    /// Whether a challenger can still dispute this proposal.
    ///
    /// A proven proposal stays challengeable until its window closes: an accepted lane only
    /// finalizes an unchallenged root, so a challenge still forces the full threshold.
    #[must_use]
    pub const fn is_challengeable(self) -> bool {
        matches!(
            self,
            Self::Unchallenged | Self::UnchallengedAndValidProofProvided
        )
    }
}

#[derive(Debug, Error)]
pub enum ProposalStatusError {
    #[error("Invalid proposal status: {0}")]
    InvalidProposalStatus(u8),
}

impl TryFrom<u8> for ProposalStatus {
    type Error = ProposalStatusError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unchallenged),
            1 => Ok(Self::Challenged),
            2 => Ok(Self::UnchallengedAndValidProofProvided),
            3 => Ok(Self::ChallengedAndValidProofProvided),
            4 => Ok(Self::Resolved),
            _ => Err(ProposalStatusError::InvalidProposalStatus(value)),
        }
    }
}

/// Domain constants committed into every root id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofDomain {
    /// World Chain L2 chain id.
    pub chain_id: u64,
    /// Proof-system encoding version.
    pub proof_system_version: u64,
    /// Hash of the rollup config and World Chain hardfork schedule.
    pub rollup_config_hash: B256,
    /// Distance in L2 blocks between parent and proposed roots.
    pub block_interval: u64,
}

impl ProofDomain {
    /// Compute the Solidity-compatible domain hash.
    #[must_use]
    pub fn hash(self) -> B256 {
        let encoded = (
            U256::from(self.chain_id),
            U256::from(self.proof_system_version),
            self.rollup_config_hash,
            U256::from(self.block_interval),
        )
            .abi_encode_params();
        keccak256(encoded)
    }
}

/// Per-proposal commitment fields used to compute a canonical root id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalCommitment {
    /// Parent `AnchorStateRegistry` or parent game address.
    pub parent_ref: Address,
    /// Claimed OP Stack output root.
    pub root_claim: B256,
    /// L2 block number for `root_claim`.
    pub l2_block_number: u64,
    /// Retry nonce for this transition. Attempt N is only proposable once attempt N-1
    /// has been invalidated by a proof timeout.
    pub attempt: u64,
}

impl ProposalCommitment {
    /// ABI-encodes the CWIA payload `MultiProofGame` reads from its clone arguments.
    ///
    /// Must match `abi.encode(domainHash, l2BlockNumber, parentRef, attempt)` as consumed by
    /// `MultiProofGame`'s CWIA getters.
    #[must_use]
    pub fn extra_data(self, domain_hash: B256) -> Bytes {
        (
            domain_hash,
            U256::from(self.l2_block_number),
            self.parent_ref,
            U256::from(self.attempt),
        )
            .abi_encode_params()
            .into()
    }

    /// Computes the `DisputeGameFactory` UUID that identifies this proposal's game.
    ///
    /// Mirrors `DisputeGameFactory.getGameUUID`, so the proposer can look a game up without
    /// an extra round trip.
    #[must_use]
    pub fn game_uuid(self, domain_hash: B256) -> B256 {
        let encoded = (
            MULTI_PROOF_GAME_TYPE,
            self.root_claim,
            self.extra_data(domain_hash),
        )
            .abi_encode_params();
        keccak256(encoded)
    }
}

/// Per-proposal commitment fields used to compute a canonical root id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCommitment {
    /// Proposal fields supplied by the proposer.
    pub proposal: ProposalCommitment,
    /// L1 origin hash pinned by the proposal factory.
    pub l1_origin_hash: B256,
    /// L1 origin block number paired with `l1_origin_hash`.
    pub l1_origin_number: u64,
}

impl RootCommitment {
    /// Compute the `DisputeGameFactory` UUID for this proposal.
    #[must_use]
    pub fn game_uuid(self, domain_hash: B256) -> B256 {
        self.proposal.game_uuid(domain_hash)
    }

    /// Compute the Solidity-compatible root id for this proposal.
    #[must_use]
    pub fn root_id(self, domain_hash: B256) -> B256 {
        let encoded = (
            domain_hash,
            self.proposal.parent_ref,
            self.proposal.root_claim,
            U256::from(self.proposal.l2_block_number),
            self.l1_origin_hash,
            U256::from(self.l1_origin_number),
        )
            .abi_encode_params();
        keccak256(encoded)
    }
}

/// WIP-1006 proof lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ProofLane {
    /// zkVM, SNARK, or equivalent validity proof.
    ValidityProof = 0,
    /// TEE signer attestation.
    TeeAttestation = 1,
    /// Security Council attestation.
    SecurityCouncil = 2,
}

impl ProofLane {
    /// Stable telemetry representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ValidityProof => "validity_proof",
            Self::TeeAttestation => "tee_attestation",
            Self::SecurityCouncil => "security_council",
        }
    }

    /// Bit assigned to this lane in the per-root proof bitmap.
    #[must_use]
    pub const fn mask(self) -> u8 {
        1 << self as u8
    }
}

/// Encodes the compact payload `MultiProofGame.submitProofLane` takes.
///
/// Mirrors `LibProof.decodeCompact`: lane id at byte 0, reward recipient at bytes 1..21, then
/// the lane-specific bytes its verifier consumes. Packed, not ABI-encoded — a leading offset
/// word would shift every field the assembly decoder reads.
#[must_use]
pub fn encode_compact_proof(lane: ProofLane, recipient: Address, proof: &[u8]) -> Bytes {
    let mut encoded = Vec::with_capacity(PROOF_HEADER_LENGTH + proof.len());
    encoded.push(lane as u8);
    encoded.extend_from_slice(recipient.as_slice());
    encoded.extend_from_slice(proof);
    encoded.into()
}

/// Count distinct lanes in a proof bitmap.
#[must_use]
pub const fn proof_count(bitmap: u8) -> u8 {
    let mut count = 0;
    let mut index = 0;
    while index < PROOF_LANE_COUNT {
        if bitmap & (1 << index) != 0 {
            count += 1;
        }
        index += 1;
    }
    count
}

/// Whether a bitmap satisfies the WIP-1006 threshold.
#[must_use]
pub const fn has_threshold(bitmap: u8) -> bool {
    proof_count(bitmap) >= PROOF_THRESHOLD
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationReason {
    None,
    ProofTimeout,
    InvalidParent,
}

#[derive(Debug, Error)]
pub enum InvalidationReasonError {
    #[error("Invalid invalidation reason: {0}")]
    InvalidReason(u8),
}

impl TryFrom<u8> for InvalidationReason {
    type Error = InvalidationReasonError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(InvalidationReason::None),
            1 => Ok(InvalidationReason::ProofTimeout),
            2 => Ok(InvalidationReason::InvalidParent),
            _ => Err(InvalidationReasonError::InvalidReason(value)),
        }
    }
}

/// A game's stored status and its current resolution evaluation, read at the same block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionStatus {
    /// Actual on-chain `status()`, which only becomes terminal after `resolve()` succeeds.
    pub status: GameStatus,
    /// Whether `resolve()` can succeed now.
    pub resolvable: bool,
    /// Predicted or resolved outcome. A local proof timeout can be terminal while the
    /// stored status is still `InProgress` and resolution is waiting for the parent.
    pub outcome: GameStatus,
    /// Current invalidation reason; a local timeout may later become `InvalidParent`.
    pub invalidation_reason: InvalidationReason,
}

impl ResolutionStatus {
    /// Whether resolving now would uphold the root claim.
    pub fn positive_resolvable(&self) -> bool {
        self.resolvable && self.outcome == GameStatus::DefenderWins
    }

    /// Returns whether the game can be resolved as invalid because its parent is invalid.
    pub fn invalid_parent_resolvable(&self) -> bool {
        self.resolvable
            && self.outcome == GameStatus::ChallengerWins
            && self.invalidation_reason == InvalidationReason::InvalidParent
    }

    /// Returns whether the game has already reached a terminal state.
    ///
    /// Neither the predicted outcome nor `resolvable` establishes actual resolution.
    pub fn is_resolved(&self) -> bool {
        self.status != GameStatus::InProgress
    }
}

/// Decoded `MultiProofGame.claimData()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimData {
    pub status: ProposalStatus,
    /// Zero when the proposal has not been disputed.
    pub challenger: Address,
    /// Challenge deadline while unchallenged, proof deadline once challenged.
    pub deadline: u64,
    pub proof_bitmap: u8,
    pub invalidation_reason: InvalidationReason,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, hex};

    #[test]
    fn lane_bitmap_counts_distinct_lanes() {
        let bitmap = ProofLane::ValidityProof.mask() | ProofLane::SecurityCouncil.mask();

        assert_eq!(proof_count(bitmap), 2);
        assert!(has_threshold(bitmap));
        assert!(!has_threshold(ProofLane::TeeAttestation.mask()));
    }

    #[test]
    fn game_status_matches_op_stack_ordering() {
        // `resolutionStatus().outcome` and `status()` are OP Stack `GameStatus`, not a
        // proposal-level state: decoding them with any other ordering silently mislabels
        // every game.
        assert_eq!(GameStatus::try_from(0).unwrap(), GameStatus::InProgress);
        assert_eq!(GameStatus::try_from(1).unwrap(), GameStatus::ChallengerWins);
        assert_eq!(GameStatus::try_from(2).unwrap(), GameStatus::DefenderWins);
        assert!(GameStatus::try_from(3).is_err());
    }

    #[test]
    fn proposal_status_matches_contract_ordering() {
        // Mirrors `IMultiProofGame.ProposalStatus`.
        assert_eq!(
            ProposalStatus::try_from(0).unwrap(),
            ProposalStatus::Unchallenged
        );
        assert_eq!(
            ProposalStatus::try_from(1).unwrap(),
            ProposalStatus::Challenged
        );
        assert_eq!(
            ProposalStatus::try_from(2).unwrap(),
            ProposalStatus::UnchallengedAndValidProofProvided
        );
        assert_eq!(
            ProposalStatus::try_from(3).unwrap(),
            ProposalStatus::ChallengedAndValidProofProvided
        );
        assert_eq!(
            ProposalStatus::try_from(4).unwrap(),
            ProposalStatus::Resolved
        );
        assert!(ProposalStatus::try_from(5).is_err());

        // A challenger may still dispute a proposal that already carries one lane.
        assert!(ProposalStatus::UnchallengedAndValidProofProvided.is_challengeable());
        assert!(!ProposalStatus::Challenged.is_challengeable());
    }

    #[test]
    fn resolution_status_reads_outcomes_not_proposal_states() {
        let finalized = ResolutionStatus {
            status: GameStatus::InProgress,
            resolvable: true,
            outcome: GameStatus::DefenderWins,
            invalidation_reason: InvalidationReason::None,
        };
        assert!(finalized.positive_resolvable());
        assert!(!finalized.is_resolved());

        let live = ResolutionStatus {
            status: GameStatus::InProgress,
            resolvable: false,
            outcome: GameStatus::InProgress,
            invalidation_reason: InvalidationReason::None,
        };
        assert!(!live.positive_resolvable());
        assert!(!live.is_resolved());

        let bad_parent = ResolutionStatus {
            status: GameStatus::InProgress,
            resolvable: true,
            outcome: GameStatus::ChallengerWins,
            invalidation_reason: InvalidationReason::InvalidParent,
        };
        assert!(bad_parent.invalid_parent_resolvable());
    }

    #[test]
    fn local_timeout_is_not_resolved_until_the_stored_status_changes() {
        let waiting = ResolutionStatus {
            status: GameStatus::InProgress,
            resolvable: false,
            outcome: GameStatus::ChallengerWins,
            invalidation_reason: InvalidationReason::ProofTimeout,
        };
        assert!(!waiting.is_resolved());
        assert!(!waiting.positive_resolvable());
        assert!(!waiting.invalid_parent_resolvable());

        let ready = ResolutionStatus {
            resolvable: true,
            ..waiting
        };
        assert!(!ready.is_resolved());
        let resolved = ResolutionStatus {
            status: GameStatus::ChallengerWins,
            ..waiting
        };
        assert!(resolved.is_resolved());
        let defender_wins = ResolutionStatus {
            status: GameStatus::DefenderWins,
            outcome: GameStatus::DefenderWins,
            invalidation_reason: InvalidationReason::None,
            ..waiting
        };
        assert!(defender_wins.is_resolved());
    }

    #[test]
    fn compact_proof_matches_lib_proof_header_layout() {
        let recipient = address!("00000000000000000000000000000000000000aa");
        let encoded = encode_compact_proof(ProofLane::TeeAttestation, recipient, &[0xbb, 0xcc]);

        // `LibProof.decodeCompact` reads the lane id from byte 0 and the recipient from the
        // next 20 bytes with no padding; an ABI encoding would offset both.
        assert_eq!(encoded[0], ProofLane::TeeAttestation as u8);
        assert_eq!(&encoded[1..PROOF_HEADER_LENGTH], recipient.as_slice());
        assert_eq!(&encoded[PROOF_HEADER_LENGTH..], &[0xbb, 0xcc]);
    }

    #[test]
    fn empty_lane_proof_still_carries_a_full_header() {
        // `submitProofLane` rejects anything shorter than the header before decoding it.
        let encoded = encode_compact_proof(ProofLane::ValidityProof, Address::ZERO, &[]);

        assert_eq!(encoded.len(), PROOF_HEADER_LENGTH);
    }

    #[test]
    fn root_id_changes_when_domain_changes() {
        let domain = ProofDomain {
            chain_id: 4801,
            proof_system_version: PROOF_SYSTEM_VERSION,
            rollup_config_hash: b256!(
                "1111111111111111111111111111111111111111111111111111111111111111"
            ),
            block_interval: 10,
        };
        let proposal = ProposalCommitment {
            parent_ref: address!("0000000000000000000000000000000000001006"),
            root_claim: b256!("2222222222222222222222222222222222222222222222222222222222222222"),
            l2_block_number: 10,
            attempt: 0,
        };
        let commitment = RootCommitment {
            proposal,
            l1_origin_hash: b256!(
                "3333333333333333333333333333333333333333333333333333333333333333"
            ),
            l1_origin_number: 1,
        };

        // Reference values from `cast abi-encode` / `cast keccak`, pinning these encodings to
        // `LibProof.domainHash` and `LibProof.rootId`.
        assert_eq!(
            domain.hash(),
            b256!("2eadd7e0cde9ca6f758216655e263e8d197480ebc4d3478403000447fe62f4be")
        );
        let root_id = commitment.root_id(domain.hash());
        assert_eq!(
            root_id,
            b256!("6cccba67d43368ae81da6cf22798e228b82b953c51c1bbce76959b166b888dd3")
        );

        assert_ne!(B256::ZERO, proposal.game_uuid(domain.hash()));
        let changed = ProofDomain {
            chain_id: 4802,
            ..domain
        };

        assert_ne!(root_id, commitment.root_id(changed.hash()));
    }

    #[test]
    fn game_uuid_matches_dispute_game_factory_encoding() {
        let domain_hash = b256!("1111111111111111111111111111111111111111111111111111111111111111");
        let proposal = ProposalCommitment {
            parent_ref: address!("0000000000000000000000000000000000001006"),
            root_claim: b256!("2222222222222222222222222222222222222222222222222222222222222222"),
            l2_block_number: 100,
            attempt: 0,
        };

        // Reference values from `cast abi-encode` / `cast keccak`, pinning this encoding to the
        // CWIA payload `MultiProofGame` reads at offsets 0x54/0x74/0x94/0xB4 and to
        // `DisputeGameFactory.getGameUUID(GameType,Claim,bytes)`.
        assert_eq!(
            proposal.extra_data(domain_hash),
            Bytes::from_static(&hex!(
                "1111111111111111111111111111111111111111111111111111111111111111"
                "0000000000000000000000000000000000000000000000000000000000000064"
                "0000000000000000000000000000000000000000000000000000000000001006"
                "0000000000000000000000000000000000000000000000000000000000000000"
            ))
        );
        assert_eq!(
            proposal.game_uuid(domain_hash),
            b256!("97c09945ea02810652360265a6c8f070ce4d6fb10651f7f76126130d6209ee33")
        );

        // Retries occupy a distinct factory UUID.
        let retry = ProposalCommitment {
            attempt: 1,
            ..proposal
        };
        assert_ne!(
            proposal.game_uuid(domain_hash),
            retry.game_uuid(domain_hash)
        );
    }
}
