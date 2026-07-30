use alloy_primitives::{Address, B256, BlockNumber, Bytes, U256, keccak256};
use alloy_sol_types::SolValue;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Number of distinct lanes required by WIP-1006 for challenged finality.
pub const PROOF_THRESHOLD: u8 = 2;

/// Number of configured proof lanes.
pub const PROOF_LANE_COUNT: u8 = 3;

/// Version of the World Chain proof-domain encoding implemented here.
///
/// Bumped to 2 when `rootId` gained the explicit pre-state commitment; proofs produced against
/// version 1 do not carry over.
pub const PROOF_SYSTEM_VERSION: u64 = 2;

/// OP Stack dispute-game type allocated to WIP-1006 (`GameTypes.MULTI_PROOF_GAME_TYPE`).
///
/// The stock `DisputeGameFactory` indexes every game type in one array, so every
/// index-based read must filter on this value.
pub const MULTI_PROOF_GAME_TYPE: u32 = 1006;

/// The `MultiProofGame.WorldChainGameCreated` event.
#[derive(Debug, Clone, Copy)]
pub struct WorldChainGameCreated {
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

/// A game root state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootState {
    None,
    Proposed,
    Challenged,
    Finalized,
    Invalidated,
}

#[derive(Debug, Error)]
pub enum RootStateError {
    #[error("Invalid root state: {0}")]
    InvalieRootState(u8),
}

impl TryFrom<u8> for RootState {
    type Error = RootStateError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(RootState::None),
            1 => Ok(RootState::Proposed),
            2 => Ok(RootState::Challenged),
            3 => Ok(RootState::Finalized),
            4 => Ok(RootState::Invalidated),
            _ => Err(RootStateError::InvalieRootState(value)),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Must match the dynamic `extraData` consumed by `MultiProofGame`.
    #[must_use]
    pub fn extra_data(
        self,
        domain_hash: B256,
        retry_of: Address,
        l1_origin_hash: B256,
        l1_origin_number: u64,
        creation_proof: Bytes,
    ) -> Bytes {
        (
            domain_hash,
            U256::from(self.l2_block_number),
            self.parent_ref,
            U256::from(self.attempt),
            retry_of,
            l1_origin_hash,
            U256::from(l1_origin_number),
            creation_proof,
        )
            .abi_encode_params()
            .into()
    }
}

/// Proposal fields decoded from a WIP-1006 game's factory `extraData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalExtraData {
    pub domain_hash: B256,
    pub l2_block_number: u64,
    pub parent_ref: Address,
    pub attempt: u64,
    pub retry_of: Address,
    pub l1_origin_hash: B256,
    pub l1_origin_number: u64,
    pub creation_proof: Bytes,
}

impl ProposalExtraData {
    /// Decodes the canonical proof-backed proposal payload.
    pub fn decode(data: &[u8]) -> Result<Self, String> {
        let decoded =
            <(B256, U256, Address, U256, Address, B256, U256, Bytes)>::abi_decode_params(data)
                .map_err(|error| error.to_string())?;
        let canonical = decoded.clone().abi_encode_params();
        if canonical != data {
            return Err("non-canonical proposal extraData".to_string());
        }
        Ok(Self {
            domain_hash: decoded.0,
            l2_block_number: decoded
                .1
                .try_into()
                .map_err(|_| "l2 block number exceeds u64".to_string())?,
            parent_ref: decoded.2,
            attempt: decoded
                .3
                .try_into()
                .map_err(|_| "attempt exceeds u64".to_string())?,
            retry_of: decoded.4,
            l1_origin_hash: decoded.5,
            l1_origin_number: decoded
                .6
                .try_into()
                .map_err(|_| "l1 origin number exceeds u64".to_string())?,
            creation_proof: decoded.7,
        })
    }
}

/// Per-proposal commitment fields used to compute a canonical root id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCommitment {
    /// Proposal fields supplied by the proposer.
    pub proposal: ProposalCommitment,
    /// Output root the transition starts from, as snapshotted by the game at creation.
    pub starting_root_claim: B256,
    /// L2 block number the transition starts from.
    pub starting_l2_block_number: u64,
    /// Proposer-selected L1 origin hash verified by the game at creation.
    pub l1_origin_hash: B256,
    /// L1 origin block number paired with `l1_origin_hash`.
    pub l1_origin_number: u64,
}

impl RootCommitment {
    /// Compute the Solidity-compatible root id for this proposal.
    ///
    /// Must match `ProofLib.rootId`. The pre-state is committed explicitly because `parent_ref`
    /// alone does not pin it: for the anchor-registry sentinel the address is fixed while the
    /// anchor value moves.
    #[must_use]
    pub fn root_id(self, domain_hash: B256) -> B256 {
        let encoded = (
            domain_hash,
            self.proposal.parent_ref,
            self.starting_root_claim,
            U256::from(self.starting_l2_block_number),
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
    /// Bit assigned to this lane in the per-root proof bitmap.
    #[must_use]
    pub const fn mask(self) -> u8 {
        1 << self as u8
    }
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
    Blacklisted,
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
            3 => Ok(InvalidationReason::Blacklisted),
            _ => Err(InvalidationReasonError::InvalidReason(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionStatus {
    pub resolvable: bool,
    pub root_state: RootState,
    pub invalidation_reason: InvalidationReason,
}

impl ResolutionStatus {
    /// Returns true whether this resolution status is positive resolvable:
    /// - `resolvable` is true AND
    /// - the expected root state outcome is `Finalized`.
    pub fn positive_resolvable(&self) -> bool {
        self.resolvable && self.root_state == RootState::Finalized
    }

    /// Returns whether the game has already reached a terminal state.
    ///
    /// The root state may describe the expected outcome of a game that is currently resolvable,
    /// so a terminal root state is considered resolved only when `resolvable` is false.
    pub fn is_resolved(&self) -> bool {
        !self.resolvable
            && (self.root_state == RootState::Finalized
                || self.root_state == RootState::Invalidated)
    }
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
            starting_root_claim: b256!(
                "1111111111111111111111111111111111111111111111111111111111111111"
            ),
            starting_l2_block_number: 0,
            l1_origin_hash: b256!(
                "3333333333333333333333333333333333333333333333333333333333333333"
            ),
            l1_origin_number: 1,
        };

        // Reference values from `cast abi-encode` / `cast keccak`, pinning these encodings to
        // `ProofLib.domainHash` and `ProofLib.rootId`.
        assert_eq!(
            domain.hash(),
            b256!("b0b28d77aae793918b08a343f71435e5abbaeceaab90a99e98f32a2563ec6386")
        );
        let root_id = commitment.root_id(domain.hash());
        assert_eq!(
            root_id,
            b256!("a6af84008c0ba6b09b25c6a1b944e6d995223057eda1a0fbc79dcff9a7ba140a")
        );

        let changed = ProofDomain {
            chain_id: 4802,
            ..domain
        };

        assert_ne!(root_id, commitment.root_id(changed.hash()));
    }

    #[test]
    fn proposal_extra_data_round_trips() {
        let domain_hash = b256!("1111111111111111111111111111111111111111111111111111111111111111");
        let proposal = ProposalCommitment {
            parent_ref: address!("0000000000000000000000000000000000001006"),
            root_claim: b256!("2222222222222222222222222222222222222222222222222222222222222222"),
            l2_block_number: 100,
            attempt: 0,
        };

        let l1_origin_hash =
            b256!("3333333333333333333333333333333333333333333333333333333333333333");
        let proof = Bytes::from_static(&hex!("deadbeef"));
        let encoded = proposal.extra_data(
            domain_hash,
            Address::ZERO,
            l1_origin_hash,
            42,
            proof.clone(),
        );
        assert_eq!(
            ProposalExtraData::decode(&encoded).unwrap(),
            ProposalExtraData {
                domain_hash,
                l2_block_number: proposal.l2_block_number,
                parent_ref: proposal.parent_ref,
                attempt: proposal.attempt,
                retry_of: Address::ZERO,
                l1_origin_hash,
                l1_origin_number: 42,
                creation_proof: proof,
            }
        );
    }
}
