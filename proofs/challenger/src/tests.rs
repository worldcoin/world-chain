use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{Address, B256, BlockNumber, U256, address};
use async_trait::async_trait;
use world_chain_proofs::{OutputRootError, OutputRootProvider};

use crate::{
    challenger::WorldChainChallenger,
    config::ChallengerConfig,
    error::ChallengerError,
    traits::ChallengerClient,
    types::{ChallengeSubmission, GameCreated, RootState},
};

const FACTORY: Address = address!("0000000000000000000000000000000000001006");
const GAME_1: Address = address!("0000000000000000000000000000000000000001");
const FINALIZED_L1_BLOCK: BlockNumber = 10_000;
const L2_BLOCK: u64 = 100;

/// State discriminant matching [`RootState::Proposed`].
const STATE_PROPOSED: u8 = 1;
/// State discriminant matching [`RootState::Challenged`].
const STATE_CHALLENGED: u8 = 2;

#[derive(Debug, Clone)]
struct MockClient {
    finalized: BlockNumber,
    games: Vec<(Address, B256, u64)>,
    states: HashMap<Address, u8>,
    deadlines: HashMap<Address, u64>,
    submissions: Arc<Mutex<Vec<Address>>>,
}

#[async_trait]
impl ChallengerClient for MockClient {
    async fn root_state(&self, game: Address) -> Result<RootState, ChallengerError> {
        let raw = self.states.get(&game).copied().unwrap_or(STATE_PROPOSED);
        RootState::try_from(raw)
    }

    async fn finalized_l1_block_num(&self) -> Result<BlockNumber, ChallengerError> {
        Ok(self.finalized)
    }

    async fn games_created(
        &self,
        _from: BlockNumber,
        _to: BlockNumber,
    ) -> Result<Vec<GameCreated>, ChallengerError> {
        Ok(self
            .games
            .iter()
            .map(|&(game, root_claim, l2_block_number)| GameCreated {
                proposal_key: B256::ZERO,
                root_it: B256::ZERO,
                game,
                proposer: Address::ZERO,
                root_claim,
                l2_block_number,
                parent_ref: Address::ZERO,
                l1_origin_hash: B256::ZERO,
                l1_origin_number: 0,
            })
            .collect())
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError> {
        Ok(self.deadlines.get(&game).copied().unwrap_or(u64::MAX))
    }

    async fn submit_challenge(
        &self,
        game: Address,
        _challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        self.submissions.lock().expect("not poisoned").push(game);
        Ok(ChallengeSubmission {
            tx_hash: B256::repeat_byte(0xaa),
        })
    }
}

#[derive(Debug, Clone)]
struct MockOutputRoots {
    roots: HashMap<u64, B256>,
}

#[async_trait]
impl OutputRootProvider for MockOutputRoots {
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, OutputRootError> {
        self.roots
            .get(&l2_block_number)
            .copied()
            .ok_or_else(|| OutputRootError::Rpc(format!("missing root for {l2_block_number}")))
    }
}

fn config() -> ChallengerConfig {
    ChallengerConfig {
        challenger_bond: U256::from(1),
        factory_contract: FACTORY,
        poll_interval: Duration::from_secs(1),
    }
}

#[tokio::test]
async fn scan_once_challenges_invalid_root() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, proposed_root, L2_BLOCK)],
        states: HashMap::new(),
        deadlines: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(L2_BLOCK, canonical_root)]),
    };
    let challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    assert_eq!(
        submissions.lock().expect("not poisoned").as_slice(),
        &[GAME_1]
    );
}

#[tokio::test]
async fn scan_once_leaves_valid_root() {
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, canonical_root, L2_BLOCK)],
        states: HashMap::new(),
        deadlines: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(L2_BLOCK, canonical_root)]),
    };
    let challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    assert!(submissions.lock().expect("not poisoned").is_empty());
}

#[tokio::test]
async fn scan_once_skips_non_proposed_game() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, proposed_root, L2_BLOCK)],
        states: HashMap::from([(GAME_1, STATE_CHALLENGED)]),
        deadlines: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(L2_BLOCK, canonical_root)]),
    };
    let challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    assert!(submissions.lock().expect("not poisoned").is_empty());
}

#[tokio::test]
async fn scan_once_skips_expired_challenge_deadline() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, proposed_root, L2_BLOCK)],
        states: HashMap::new(),
        deadlines: HashMap::from([(GAME_1, 0)]),
        submissions: Arc::clone(&submissions),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(L2_BLOCK, canonical_root)]),
    };
    let challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    assert!(submissions.lock().expect("not poisoned").is_empty());
}
