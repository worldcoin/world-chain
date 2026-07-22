use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{Address, B256, BlockNumber, U256, address, b256};
use async_trait::async_trait;
use world_chain_proofs::{
    ConsensusError, ConsensusProvider, InvalidationReason, ProposalCommitment, ResolutionStatus,
    RootState,
};

use crate::{
    ParentRef, Proposal, ProposalSubmission, ProposerClient, ProposerConfig, ProposerError,
    WorldChainProposer,
    types::{CloseGameSubmission, ResolveSubmission, WithdrawSubmission},
};

const DOMAIN_HASH: B256 = b256!("1111111111111111111111111111111111111111111111111111111111111111");
const ANCHOR: Address = address!("0000000000000000000000000000000000001006");
const GAME_1: Address = address!("0000000000000000000000000000000000000001");

#[derive(Debug, Clone)]
struct MockContracts {
    anchor: ParentRef,
    games: HashMap<B256, Address>,
    submissions: Arc<Mutex<Vec<Proposal>>>,
}

#[async_trait]
impl ProposerClient for MockContracts {
    async fn anchor_parent(&self) -> Result<ParentRef, ProposerError> {
        Ok(self.anchor)
    }

    async fn proposal_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError> {
        Ok(commitment.proposal_key(DOMAIN_HASH))
    }

    async fn game_for_proposal_key(
        &self,
        proposal_key: B256,
    ) -> Result<Option<Address>, ProposerError> {
        Ok(self.games.get(&proposal_key).copied())
    }

    async fn resolution_status(&self, _game: Address) -> Result<ResolutionStatus, ProposerError> {
        Ok(ResolutionStatus {
            resolvable: false,
            root_state: RootState::Proposed,
            invalidation_reason: InvalidationReason::None,
        })
    }

    async fn resolve_game(&self, _game: Address) -> Result<ResolveSubmission, ProposerError> {
        Ok(ResolveSubmission {
            tx_hash: B256::repeat_byte(0xbb),
        })
    }

    async fn close_game(&self, _game: Address) -> Result<CloseGameSubmission, ProposerError> {
        Ok(CloseGameSubmission {
            tx_hash: B256::repeat_byte(0xcc),
        })
    }

    async fn claimable(&self, _game: Address) -> Result<U256, ProposerError> {
        Ok(U256::ZERO)
    }

    async fn withdraw(&self, _game: Address) -> Result<WithdrawSubmission, ProposerError> {
        Ok(WithdrawSubmission {
            tx_hash: B256::repeat_byte(0xdd),
            amount: U256::ZERO,
        })
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        _proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError> {
        self.submissions
            .lock()
            .expect("not poisoned")
            .push(*proposal);
        Ok(ProposalSubmission {
            tx_hash: B256::repeat_byte(0xaa),
            game_address: Address::repeat_byte(0xaa),
        })
    }
}

#[derive(Debug, Clone)]
struct MockOutputRoots {
    roots: HashMap<u64, B256>,
    finalized_l2_block: BlockNumber,
}

#[async_trait]
impl ConsensusProvider for MockOutputRoots {
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ConsensusError> {
        self.roots
            .get(&l2_block_number)
            .copied()
            .ok_or_else(|| ConsensusError::Rpc(format!("missing root for {l2_block_number}")))
    }

    async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError> {
        Ok(self.finalized_l2_block)
    }
}

fn config() -> ProposerConfig {
    ProposerConfig {
        block_interval: 10,
        proposer_bond: U256::from(1),
        poll_interval: Duration::from_secs(1),
    }
}

fn proposal_key(parent_ref: Address, root_claim: B256, l2_block_number: u64) -> B256 {
    ProposalCommitment {
        parent_ref,
        root_claim,
        l2_block_number,
    }
    .proposal_key(DOMAIN_HASH)
}

#[tokio::test]
async fn anchor_and_canonical_line_walks_existing_games_until_gap() {
    let root_10 = B256::repeat_byte(0x10);
    let root_20 = B256::repeat_byte(0x20);
    let mut games = HashMap::new();
    games.insert(proposal_key(ANCHOR, root_10, 10), GAME_1);

    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games,
        submissions: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10), (20, root_20)]),
        finalized_l2_block: 20,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.canonical_line().games(),
        &[ParentRef {
            address: GAME_1,
            l2_block_number: 10,
        }]
    );
}

#[tokio::test]
async fn propose_submits_proposal_after_last_canonical_game() {
    let submissions = Arc::default();
    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 10,
    };
    let mut proposer = WorldChainProposer::new(config(), contracts, output_roots);
    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    proposer.propose(&canonical_scan).await.unwrap();

    let proposal = submissions.lock().expect("not poisoned")[0];
    assert_eq!(proposal.parent_ref, ANCHOR);
    assert_eq!(proposal.root_claim, B256::repeat_byte(0x10));
    assert_eq!(proposal.l2_block_number, 10);
    assert_eq!(
        proposal.proposal_key,
        proposal_key(ANCHOR, B256::repeat_byte(0x10), 10)
    );
}

#[tokio::test]
async fn zero_block_interval_is_rejected() {
    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::default(),
    };
    let proposer = WorldChainProposer::new(
        ProposerConfig {
            block_interval: 0,
            ..config()
        },
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
    );

    assert!(matches!(
        proposer.anchor_and_canonical_line().await,
        Err(ProposerError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn anchor_and_canonical_line_stops_at_finalized_l2_block() {
    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 9,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert!(canonical_scan.canonical_line().games().is_empty());
    assert_eq!(
        canonical_scan.canonical_line().anchor(),
        ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        }
    );
}
