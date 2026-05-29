//! World Chain proposer primitives.
//!
//! The proposer watches L2 output roots and creates `WorldChainProofSystemGame`
//! contracts on L1 through `WorldChainProofSystemFactory`.

use std::time::Duration;

use alloy_primitives::{Address, B256, TxHash, U256};
use alloy_provider::Provider;
use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;
use tracing::{info, warn};
use world_chain_proofs::{
    IWorldChainAnchorStateRegistry, IWorldChainProofSystemFactory, IWorldChainProofSystemGame,
    ProposalCommitment,
};

/// Configuration for the proposer loop.
#[derive(Debug, Clone)]
pub struct ProposerConfig {
    /// Number of L2 blocks between proposals.
    pub block_interval: u64,
    /// Bond sent with `WorldChainProofSystemFactory.propose`.
    pub proposer_bond: U256,
    /// Delay between periodic proposal attempts.
    pub poll_interval: Duration,
}

impl ProposerConfig {
    fn validate(&self) -> Result<(), ProposerError> {
        if self.block_interval == 0 {
            return Err(ProposerError::InvalidConfig(
                "block_interval must be greater than zero",
            ));
        }
        if self.poll_interval.is_zero() {
            return Err(ProposerError::InvalidConfig(
                "poll_interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for ProposerConfig {
    fn default() -> Self {
        Self {
            block_interval: 0,
            proposer_bond: U256::ZERO,
            poll_interval: Duration::from_secs(12),
        }
    }
}

/// A parent reference that the next proposal should build on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentRef {
    /// Address of the anchor registry or parent game.
    pub address: Address,
    /// L2 block number of the parent output root.
    pub l2_block_number: u64,
}

/// Candidate proposal data supplied to the proof-system factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proposal {
    /// Address of the anchor registry or parent game.
    pub parent_ref: Address,
    /// Claimed OP Stack output root.
    pub root_claim: B256,
    /// L2 block number for `root_claim`.
    pub l2_block_number: u64,
    /// Commitment to ordered intermediate roots. Currently always zero.
    pub intermediate_roots_hash: B256,
    /// Deterministic factory lookup key, excluding L1 origin.
    pub proposal_key: B256,
}

impl Proposal {
    /// Returns the proposal commitment used to compute the factory lookup key.
    #[must_use]
    pub const fn commitment(&self) -> ProposalCommitment {
        ProposalCommitment {
            parent_ref: self.parent_ref,
            root_claim: self.root_claim,
            l2_block_number: self.l2_block_number,
            intermediate_roots_hash: self.intermediate_roots_hash,
        }
    }
}

/// Result of a submitted proposal transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalSubmission {
    /// Transaction hash for the proposal submission.
    pub tx_hash: TxHash,
}

/// Errors returned by the proposer.
#[derive(Debug, Error)]
pub enum ProposerError {
    /// Invalid proposer configuration.
    #[error("invalid proposer config: {0}")]
    InvalidConfig(&'static str),
    /// Adding `block_interval` overflowed `u64`.
    #[error("l2 block number overflow: parent {parent_block} + interval {block_interval}")]
    BlockNumberOverflow {
        /// Parent L2 block number.
        parent_block: u64,
        /// Configured block interval.
        block_interval: u64,
    },
    /// Contract call or transaction failure.
    #[error("contract error: {0}")]
    Contract(String),
    /// RPC transport or JSON-RPC failure.
    #[error("rpc error: {0}")]
    Rpc(String),
    /// The output-root RPC response did not contain an output root.
    #[error("optimism_outputAtBlock response did not contain an output root")]
    MissingOutputRoot,
}

/// Minimal contract surface needed by the proposer.
#[async_trait]
pub trait ProofSystemClient: Send + Sync {
    /// Reads the parent state from the anchor registry.
    async fn anchor_parent(&self) -> Result<ParentRef, ProposerError>;

    /// Computes the deterministic proposal key used by the factory lookup.
    async fn proposal_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError>;

    /// Returns an existing game for `proposal_key`, if one exists.
    async fn game_for_proposal_key(
        &self,
        proposal_key: B256,
    ) -> Result<Option<Address>, ProposerError>;

    /// Submits a proposal transaction to the factory.
    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError>;
}

/// Source for OP Stack output roots.
#[async_trait]
pub trait OutputRootProvider: Send + Sync {
    /// Returns the output root for an L2 block number.
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ProposerError>;
}

/// Library-only proposer implementation.
#[derive(Debug)]
pub struct WorldChainProposer<C, O> {
    config: ProposerConfig,
    contracts: C,
    output_roots: O,
}

impl<C, O> WorldChainProposer<C, O> {
    /// Creates a proposer from contract and output-root clients.
    pub const fn new(config: ProposerConfig, contracts: C, output_roots: O) -> Self {
        Self {
            config,
            contracts,
            output_roots,
        }
    }

    /// Returns the proposer configuration.
    #[must_use]
    pub const fn config(&self) -> &ProposerConfig {
        &self.config
    }
}

impl<C, O> WorldChainProposer<C, O>
where
    C: ProofSystemClient,
    O: OutputRootProvider,
{
    /// Finds the first missing proposal after the current anchor.
    pub async fn prepare_next_proposal(&self) -> Result<Proposal, ProposerError> {
        self.config.validate()?;

        let mut parent = self.contracts.anchor_parent().await?;

        loop {
            let l2_block_number = parent
                .l2_block_number
                .checked_add(self.config.block_interval)
                .ok_or(ProposerError::BlockNumberOverflow {
                    parent_block: parent.l2_block_number,
                    block_interval: self.config.block_interval,
                })?;

            let root_claim = self
                .output_roots
                .output_root_at_block(l2_block_number)
                .await?;
            let mut proposal = Proposal {
                parent_ref: parent.address,
                root_claim,
                l2_block_number,
                intermediate_roots_hash: B256::ZERO,
                proposal_key: B256::ZERO,
            };
            proposal.proposal_key = self.contracts.proposal_key(proposal.commitment()).await?;

            if let Some(game) = self
                .contracts
                .game_for_proposal_key(proposal.proposal_key)
                .await?
            {
                parent = ParentRef {
                    address: game,
                    l2_block_number,
                };
                continue;
            }

            return Ok(proposal);
        }
    }

    /// Prepares and submits one proposal, if the next expected game is missing.
    pub async fn propose_once(&self) -> Result<(Proposal, ProposalSubmission), ProposerError> {
        let proposal = self.prepare_next_proposal().await?;
        let submission = self
            .contracts
            .submit_proposal(&proposal, self.config.proposer_bond)
            .await?;
        Ok((proposal, submission))
    }

    /// Runs the proposer forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&self) -> Result<(), ProposerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            match self.propose_once().await {
                Ok((proposal, submission)) => {
                    info!(
                        l2_block_number = proposal.l2_block_number,
                        parent_ref = %proposal.parent_ref,
                        proposal_key = ?proposal.proposal_key,
                        tx_hash = ?submission.tx_hash,
                        "submitted World Chain proof-system game"
                    );
                }
                Err(error) => {
                    warn!(%error, "proposal attempt failed");
                }
            }
        }
    }
}

/// Alloy-backed implementation of [`ProofSystemClient`].
#[derive(Debug, Clone)]
pub struct AlloyProofSystemClient<P> {
    factory: IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance<P>,
    anchor: IWorldChainAnchorStateRegistry::IWorldChainAnchorStateRegistryInstance<P>,
    provider: P,
    anchor_address: Address,
}

impl<P> AlloyProofSystemClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address, anchor_address: Address) -> Self {
        let factory = IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let anchor = IWorldChainAnchorStateRegistry::IWorldChainAnchorStateRegistryInstance::new(
            anchor_address,
            provider.clone(),
        );

        Self {
            factory,
            anchor,
            provider,
            anchor_address,
        }
    }
}

#[async_trait]
impl<P> ProofSystemClient for AlloyProofSystemClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn anchor_parent(&self) -> Result<ParentRef, ProposerError> {
        let l2_block_number = self
            .anchor
            .currentL2BlockNumber()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok(ParentRef {
            address: self.anchor_address,
            l2_block_number: u256_to_u64(l2_block_number, "currentL2BlockNumber")?,
        })
    }

    async fn proposal_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError> {
        self.factory
            .computeProposalKey(
                commitment.parent_ref,
                commitment.root_claim,
                U256::from(commitment.l2_block_number),
                commitment.intermediate_roots_hash,
            )
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn game_for_proposal_key(
        &self,
        proposal_key: B256,
    ) -> Result<Option<Address>, ProposerError> {
        let game = self
            .factory
            .games(proposal_key)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok((game != Address::ZERO).then_some(game))
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError> {
        let pending = self
            .factory
            .propose(
                proposal.parent_ref,
                proposal.root_claim,
                U256::from(proposal.l2_block_number),
                proposal.intermediate_roots_hash,
            )
            .value(proposer_bond)
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let tx_hash = *pending.tx_hash();
        pending
            .watch()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok(ProposalSubmission { tx_hash })
    }
}

impl<P> AlloyProofSystemClient<P>
where
    P: Provider + Clone,
{
    /// Reads an L2 block number from a game contract.
    pub async fn game_l2_block_number(&self, game: Address) -> Result<u64, ProposerError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let l2_block_number = game
            .l2BlockNumber()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        u256_to_u64(l2_block_number, "l2BlockNumber")
    }
}

/// HTTP client for `optimism_outputAtBlock`.
#[derive(Debug, Clone)]
pub struct OptimismOutputRootClient {
    client: reqwest::Client,
    rpc_url: String,
}

impl OptimismOutputRootClient {
    /// Creates a new output-root client.
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            rpc_url: rpc_url.into(),
        }
    }
}

#[async_trait]
impl OutputRootProvider for OptimismOutputRootClient {
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ProposerError> {
        let block = format!("0x{l2_block_number:x}");
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "optimism_outputAtBlock",
            "params": [block],
        });

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&request)
            .send()
            .await
            .map_err(|error| ProposerError::Rpc(error.to_string()))?
            .error_for_status()
            .map_err(|error| ProposerError::Rpc(error.to_string()))?
            .json::<JsonRpcResponse<OutputAtBlockResponse>>()
            .await
            .map_err(|error| ProposerError::Rpc(error.to_string()))?;

        if let Some(error) = response.error {
            return Err(ProposerError::Rpc(format!(
                "json-rpc error {}: {}",
                error.code, error.message
            )));
        }

        let output = response.result.ok_or(ProposerError::MissingOutputRoot)?;
        output
            .output_root
            .parse()
            .map_err(|error| ProposerError::Rpc(format!("invalid outputRoot: {error}")))
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct OutputAtBlockResponse {
    #[serde(rename = "outputRoot")]
    output_root: String,
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, ProposerError> {
    value
        .try_into()
        .map_err(|_| ProposerError::Contract(format!("{field} overflows u64")))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use alloy_primitives::{address, b256};

    use super::*;

    const DOMAIN_HASH: B256 =
        b256!("1111111111111111111111111111111111111111111111111111111111111111");
    const ANCHOR: Address = address!("0000000000000000000000000000000000001006");
    const GAME_1: Address = address!("0000000000000000000000000000000000000001");

    #[derive(Debug, Clone)]
    struct MockContracts {
        anchor: ParentRef,
        games: HashMap<B256, Address>,
        submissions: Arc<Mutex<Vec<Proposal>>>,
    }

    #[async_trait]
    impl ProofSystemClient for MockContracts {
        async fn anchor_parent(&self) -> Result<ParentRef, ProposerError> {
            Ok(self.anchor)
        }

        async fn proposal_key(
            &self,
            commitment: ProposalCommitment,
        ) -> Result<B256, ProposerError> {
            Ok(commitment.proposal_key(DOMAIN_HASH))
        }

        async fn game_for_proposal_key(
            &self,
            proposal_key: B256,
        ) -> Result<Option<Address>, ProposerError> {
            Ok(self.games.get(&proposal_key).copied())
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
            })
        }
    }

    #[derive(Debug, Clone)]
    struct MockOutputRoots {
        roots: HashMap<u64, B256>,
    }

    #[async_trait]
    impl OutputRootProvider for MockOutputRoots {
        async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ProposerError> {
            self.roots
                .get(&l2_block_number)
                .copied()
                .ok_or_else(|| ProposerError::Rpc(format!("missing root for {l2_block_number}")))
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
            intermediate_roots_hash: B256::ZERO,
        }
        .proposal_key(DOMAIN_HASH)
    }

    #[tokio::test]
    async fn prepare_next_proposal_walks_existing_games_until_gap() {
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
        };
        let proposer = WorldChainProposer::new(config(), contracts, output_roots);

        let proposal = proposer.prepare_next_proposal().await.unwrap();

        assert_eq!(proposal.parent_ref, GAME_1);
        assert_eq!(proposal.root_claim, root_20);
        assert_eq!(proposal.l2_block_number, 20);
        assert_eq!(proposal.intermediate_roots_hash, B256::ZERO);
        assert_eq!(proposal.proposal_key, proposal_key(GAME_1, root_20, 20));
    }

    #[tokio::test]
    async fn propose_once_submits_prepared_proposal() {
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
        };
        let proposer = WorldChainProposer::new(config(), contracts, output_roots);

        let (proposal, submission) = proposer.propose_once().await.unwrap();

        assert_eq!(submission.tx_hash, B256::repeat_byte(0xaa));
        assert_eq!(
            submissions.lock().expect("not poisoned").as_slice(),
            &[proposal]
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
            },
        );

        assert!(matches!(
            proposer.prepare_next_proposal().await,
            Err(ProposerError::InvalidConfig(_))
        ));
    }
}
