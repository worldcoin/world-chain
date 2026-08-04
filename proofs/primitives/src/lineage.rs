use crate::{
    ConsensusError, ConsensusProvider, IAnchorStateRegistry, IDisputeGameFactory, IMultiProofGame,
    InvalidationReasonError, MAX_ATTEMPT_SCAN, MULTI_PROOF_GAME_TYPE, ProposalCommitment,
    ResolutionStatus, RootState, RootStateError,
};
use alloy_primitives::{Address, B256};
use alloy_provider::Provider;
use async_trait::async_trait;
use thiserror::Error;

/// Anchor checkpoint from which the selected proposal lineage extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineageAnchor {
    /// Current anchor game, or the registry sentinel before the first game is anchored.
    pub address: Address,
    /// L2 block number committed by the anchor.
    pub l2_block_number: u64,
}

/// Expected state transition at one proposal interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineageTransition {
    /// Registry sentinel or game that the transition extends.
    pub parent_ref: Address,
    /// Canonical output root at `l2_block_number`.
    pub root_claim: B256,
    /// L2 block claimed by this transition.
    pub l2_block_number: u64,
}

/// Highest existing retry attempt for a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineageGame {
    /// Address of the game clone.
    pub address: Address,
    /// Retry nonce encoded in the game's factory key.
    pub attempt: u64,
}

/// Existing game selected for an expected state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedLineageGame {
    /// Canonical transition used to select the game.
    pub transition: LineageTransition,
    /// Highest existing attempt for the transition.
    pub game: LineageGame,
}

/// Immutable lineage configuration exposed by the registered WIP-1006 implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisteredLineageConfig {
    /// Hash of the proof domain encoded into every game factory key.
    pub domain_hash: B256,
    /// Number of L2 blocks covered by each proposal transition.
    pub block_interval: u64,
    /// Registry that owns the selected anchor checkpoint.
    pub anchor_registry: Address,
}

/// Reason selected-lineage traversal stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageStop {
    /// The next proposal interval exceeds the consensus client's finalized L2 head.
    CaughtUp {
        target_block: u64,
        finalized_block: u64,
    },
    /// No game exists for the next expected transition.
    Missing(LineageTransition),
    /// The highest attempt for the next transition is invalidated.
    Invalidated {
        transition: LineageTransition,
        game: LineageGame,
        status: ResolutionStatus,
    },
}

/// Selected games extending the current anchor and the condition at their tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedLineage {
    anchor: LineageAnchor,
    games: Vec<SelectedLineageGame>,
    stop: LineageStop,
}

impl SelectedLineage {
    #[must_use]
    pub const fn anchor(&self) -> LineageAnchor {
        self.anchor
    }

    #[must_use]
    pub fn games(&self) -> &[SelectedLineageGame] {
        &self.games
    }

    #[must_use]
    pub const fn stop(&self) -> LineageStop {
        self.stop
    }
}

/// Failure while reading or reconstructing the selected proposal lineage.
#[derive(Debug, Error)]
pub enum LineageError {
    #[error("contract error: {0}")]
    Contract(String),
    #[error("l2 block number overflow: parent {parent_block} + interval {block_interval}")]
    BlockNumberOverflow {
        parent_block: u64,
        block_interval: u64,
    },
    #[error("lineage block interval must be greater than zero")]
    ZeroBlockInterval,
    #[error("selected game {0} has unset root state")]
    UnsetGame(Address),
    #[error(transparent)]
    Consensus(#[from] ConsensusError),
    #[error(transparent)]
    InvalidRootState(#[from] RootStateError),
    #[error(transparent)]
    InvalidInvalidationReason(#[from] InvalidationReasonError),
}

/// Contract reads required to select a proposal lineage.
#[async_trait]
pub trait LineageProvider: Send + Sync {
    /// Proposal interval committed by the registered proof domain.
    fn lineage_block_interval(&self) -> u64;

    async fn lineage_anchor(&self) -> Result<LineageAnchor, LineageError>;

    async fn game_for_transition(
        &self,
        transition: LineageTransition,
    ) -> Result<Option<LineageGame>, LineageError>;

    async fn lineage_resolution_status(
        &self,
        game: Address,
    ) -> Result<ResolutionStatus, LineageError>;
}

/// Reconstructs the unique valid lineage selected by canonical output roots.
pub async fn select_lineage<E, C>(
    execution: &E,
    consensus: &C,
) -> Result<SelectedLineage, LineageError>
where
    E: LineageProvider,
    C: ConsensusProvider,
{
    let block_interval = execution.lineage_block_interval();
    if block_interval == 0 {
        return Err(LineageError::ZeroBlockInterval);
    }

    let anchor = execution.lineage_anchor().await?;
    let finalized_block = consensus.latest_l2_finalized_block().await?;
    let mut parent = anchor;
    let mut games = Vec::new();

    loop {
        let l2_block_number = parent.l2_block_number.checked_add(block_interval).ok_or(
            LineageError::BlockNumberOverflow {
                parent_block: parent.l2_block_number,
                block_interval,
            },
        )?;
        if l2_block_number > finalized_block {
            return Ok(SelectedLineage {
                anchor,
                games,
                stop: LineageStop::CaughtUp {
                    target_block: l2_block_number,
                    finalized_block,
                },
            });
        }

        let transition = LineageTransition {
            parent_ref: parent.address,
            root_claim: consensus.output_root_at_block(l2_block_number).await?,
            l2_block_number,
        };
        let Some(game) = execution.game_for_transition(transition).await? else {
            return Ok(SelectedLineage {
                anchor,
                games,
                stop: LineageStop::Missing(transition),
            });
        };
        let status = execution.lineage_resolution_status(game.address).await?;
        if status.root_state == RootState::Invalidated {
            return Ok(SelectedLineage {
                anchor,
                games,
                stop: LineageStop::Invalidated {
                    transition,
                    game,
                    status,
                },
            });
        }
        if status.root_state == RootState::None {
            return Err(LineageError::UnsetGame(game.address));
        }

        games.push(SelectedLineageGame { transition, game });
        parent = LineageAnchor {
            address: game.address,
            l2_block_number,
        };
    }
}

/// Reads the current game-or-registry anchor sentinel.
pub async fn read_lineage_anchor<P>(
    provider: &P,
    anchor: &IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
) -> Result<LineageAnchor, LineageError>
where
    P: Provider + Clone,
{
    let (anchor_root, anchor_game) = provider
        .multicall()
        .add(anchor.getAnchorRoot())
        .add(anchor.anchorGame())
        .aggregate()
        .await
        .map_err(|error| LineageError::Contract(error.to_string()))?;
    let l2_block_number = anchor_root
        .l2SequenceNumber
        .try_into()
        .map_err(|_| LineageError::Contract("getAnchorRoot overflows u64".into()))?;

    Ok(LineageAnchor {
        address: if anchor_game == Address::ZERO {
            *anchor.address()
        } else {
            anchor_game
        },
        l2_block_number,
    })
}

/// Discovers the lineage configuration from the factory's registered WIP-1006 implementation.
pub async fn read_registered_lineage_config<P>(
    provider: &P,
    factory: &IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
) -> Result<RegisteredLineageConfig, LineageError>
where
    P: Provider + Clone,
{
    let implementation_address = factory
        .gameImpls(MULTI_PROOF_GAME_TYPE)
        .call()
        .await
        .map_err(|error| LineageError::Contract(error.to_string()))?;
    if implementation_address == Address::ZERO {
        return Err(LineageError::Contract(format!(
            "dispute-game factory {} has no implementation for game type {MULTI_PROOF_GAME_TYPE}",
            factory.address()
        )));
    }

    let implementation =
        IMultiProofGame::IMultiProofGameInstance::new(implementation_address, provider.clone());
    let (domain_hash, anchor_registry, domain) = provider
        .multicall()
        .add(implementation.domainHash())
        .add(implementation.anchorStateRegistry())
        .add(implementation.domain())
        .aggregate()
        .await
        .map_err(|error| LineageError::Contract(error.to_string()))?;
    if anchor_registry == Address::ZERO {
        return Err(LineageError::Contract(
            "registered game implementation has no anchor registry".into(),
        ));
    }
    let block_interval = domain
        .blockInterval
        .try_into()
        .map_err(|_| LineageError::Contract("domain.blockInterval overflows u64".into()))?;
    if block_interval == 0 {
        return Err(LineageError::ZeroBlockInterval);
    }

    Ok(RegisteredLineageConfig {
        domain_hash,
        block_interval,
        anchor_registry,
    })
}

/// Looks up the highest sequential retry attempt for a transition.
pub async fn read_game_for_transition<P>(
    factory: &IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    domain_hash: B256,
    transition: LineageTransition,
) -> Result<Option<LineageGame>, LineageError>
where
    P: Provider + Clone,
{
    let mut latest = None;
    for attempt in 0..MAX_ATTEMPT_SCAN {
        let commitment = ProposalCommitment {
            parent_ref: transition.parent_ref,
            root_claim: transition.root_claim,
            l2_block_number: transition.l2_block_number,
            attempt,
        };
        let entry = factory
            .games(
                MULTI_PROOF_GAME_TYPE,
                transition.root_claim,
                commitment.extra_data(domain_hash),
            )
            .call()
            .await
            .map_err(|error| LineageError::Contract(error.to_string()))?;
        if entry.proxy == Address::ZERO {
            break;
        }
        latest = Some(LineageGame {
            address: entry.proxy,
            attempt,
        });
    }
    Ok(latest)
}

/// Reads and decodes a game's current resolution evaluation.
pub async fn read_lineage_resolution_status<P>(
    game: &IMultiProofGame::IMultiProofGameInstance<P>,
) -> Result<ResolutionStatus, LineageError>
where
    P: Provider + Clone,
{
    let result = game
        .resolutionStatus()
        .call()
        .await
        .map_err(|error| LineageError::Contract(error.to_string()))?;

    Ok(ResolutionStatus {
        resolvable: result.resolvable,
        root_state: result.outcome.try_into()?,
        invalidation_reason: result.reason.try_into()?,
    })
}
