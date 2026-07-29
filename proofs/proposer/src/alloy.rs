use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, B256, BlockHash, Bytes, U256};
use alloy_provider::{Provider, WalletProvider};
use alloy_sol_types::SolValue;
use async_trait::async_trait;
use world_chain_proof_core::boot::TransitionPublicValues;
use world_chain_proofs::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IMultiProofGame,
    InvalidationReasonError, MULTI_PROOF_GAME_TYPE, ProofLane, ProposalExtraData, ResolutionStatus,
    RootStateError,
};
use world_chain_prover_service::ProofData;

use crate::{
    AnchorRef, BondManagerClient, Proposal, ProposalSubmission, ProposerClient, ProposerError,
    types::{
        ClaimSubmission, CloseGameSubmission, PendingWithdrawal, ResolveSubmission, TransitionGame,
    },
};

/// Number of WIP-1006 games requested per reverse factory scan.
const GAME_SCAN_PAGE_SIZE: u64 = 64;

/// Leaves 191 blocks of transaction-inclusion headroom inside EIP-2935's 8,191-block window.
const MAX_CREATION_PROOF_AGE: u64 = 8_000;

#[derive(Debug, Clone)]
struct ScannedTransitionGame {
    factory_index: u64,
    address: Address,
    root_claim: B256,
    data: ProposalExtraData,
}

#[derive(Debug, Clone)]
struct GameScanCache {
    anchor_game: Option<Address>,
    game_count: u64,
    games: Arc<Vec<ScannedTransitionGame>>,
}

/// Returns the leaf of every explicit `retryOf` lineage in deterministic factory order.
///
/// Proof-backed `extraData` permits several UUIDs for the same logical attempt, so parallel
/// lineages are retained for the proposer to evaluate rather than collapsed by attempt number.
fn select_transition_tips(
    mut candidates: Vec<ScannedTransitionGame>,
    parent_candidates: &[Address],
) -> Vec<TransitionGame> {
    candidates.sort_by_key(|game| game.factory_index);
    let referenced: HashSet<_> = candidates
        .iter()
        .filter_map(|game| (game.data.retry_of != Address::ZERO).then_some(game.data.retry_of))
        .collect();
    let mut found = Vec::new();
    for parent_ref in parent_candidates {
        found.extend(
            candidates
                .iter()
                .filter(|game| {
                    game.data.parent_ref == *parent_ref && !referenced.contains(&game.address)
                })
                .map(|game| TransitionGame {
                    address: game.address,
                    parent_ref: *parent_ref,
                    attempt: game.data.attempt,
                }),
        );
    }
    found
}

/// Alloy-backed implementation of the proposer contract clients.
///
/// Binds the stock OP Stack `DisputeGameFactory` and `AnchorStateRegistry`; WIP-1006 games are
/// one game type among several on that factory, so every index-based read filters on
/// [`MULTI_PROOF_GAME_TYPE`].
#[derive(Debug, Clone)]
pub struct AlloyProofSystemClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    /// Domain hash of the registered game implementation, read once at construction.
    domain_hash: B256,
    game_scan_cache: Arc<Mutex<Option<GameScanCache>>>,
    provider: P,
}

impl<P> AlloyProofSystemClient<P>
where
    P: Provider + Clone,
{
    /// Connects to the deployed proof system and reads the registered implementation's domain.
    ///
    /// Fails fast when the WIP-1006 game type has no implementation registered — either the
    /// factory address is wrong or the kill switch has cleared the implementation, and in both
    /// cases no proposal can succeed.
    pub async fn new(
        provider: P,
        factory_address: Address,
        anchor_address: Address,
    ) -> Result<Self, ProposerError> {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            anchor_address,
            provider.clone(),
        );

        let game_impl = factory
            .gameImpls(MULTI_PROOF_GAME_TYPE)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if game_impl == Address::ZERO {
            return Err(ProposerError::Contract(format!(
                "dispute-game factory {factory_address} has no implementation for game type {MULTI_PROOF_GAME_TYPE}"
            )));
        }
        let domain_hash =
            IMultiProofGame::IMultiProofGameInstance::new(game_impl, provider.clone())
                .domainHash()
                .call()
                .await
                .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok(Self {
            factory,
            anchor,
            domain_hash,
            game_scan_cache: Arc::new(Mutex::new(None)),
            provider,
        })
    }

    /// Returns the domain hash of the registered game implementation.
    #[must_use]
    pub const fn domain_hash(&self) -> B256 {
        self.domain_hash
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
    }

    async fn scan_transition_games(
        &self,
        anchor_game: Option<Address>,
        game_count: u64,
    ) -> Result<Vec<ScannedTransitionGame>, ProposerError> {
        let anchor_created_at = if let Some(anchor_game) = anchor_game {
            Some(
                self.game(anchor_game)
                    .createdAt()
                    .call()
                    .await
                    .map_err(|error| ProposerError::Contract(error.to_string()))?,
            )
        } else {
            None
        };
        let mut start = game_count - 1;
        let mut games = Vec::new();
        loop {
            let page = self
                .factory
                .findLatestGames(
                    MULTI_PROOF_GAME_TYPE,
                    U256::from(start),
                    U256::from(GAME_SCAN_PAGE_SIZE),
                )
                .call()
                .await
                .map_err(|error| ProposerError::Contract(error.to_string()))?;
            if page.is_empty() {
                break;
            }

            let mut oldest_index = start;
            let mut crossed_anchor = false;
            for entry in page {
                let index = u256_to_u64(entry.index, "findLatestGames index")?;
                oldest_index = oldest_index.min(index);
                if anchor_created_at.is_some_and(|created_at| entry.timestamp < created_at) {
                    crossed_anchor = true;
                    break;
                }
                let Ok(data) = ProposalExtraData::decode(&entry.extraData) else {
                    // A re-registered game type may have older implementations with another
                    // extraData layout. They cannot belong to this implementation's domain.
                    continue;
                };
                if data.domain_hash != self.domain_hash {
                    continue;
                }
                games.push(ScannedTransitionGame {
                    factory_index: index,
                    address: Address::from_slice(&entry.metadata.as_slice()[12..]),
                    root_claim: entry.rootClaim,
                    data,
                });
            }

            if crossed_anchor || oldest_index == 0 {
                break;
            }
            start = oldest_index - 1;
        }
        Ok(games)
    }

    async fn transition_game_snapshot(
        &self,
        anchor_game: Option<Address>,
        game_count: u64,
    ) -> Result<Arc<Vec<ScannedTransitionGame>>, ProposerError> {
        let cached = self
            .game_scan_cache
            .lock()
            .map_err(|_| ProposerError::Contract("game scan cache lock poisoned".into()))?
            .clone();
        if let Some(cached) = cached
            && cached.anchor_game == anchor_game
            && cached.game_count == game_count
        {
            return Ok(cached.games);
        }

        // TODO: If full rebuilds become a bottleneck, extend an unchanged anchor's snapshot
        // from its cached factory index. Incremental reuse must also invalidate on L1 reorgs.
        let games = Arc::new(self.scan_transition_games(anchor_game, game_count).await?);
        *self
            .game_scan_cache
            .lock()
            .map_err(|_| ProposerError::Contract("game scan cache lock poisoned".into()))? =
            Some(GameScanCache {
                anchor_game,
                game_count,
                games: Arc::clone(&games),
            });
        Ok(games)
    }

    /// Resolves the WIP-1006 game at a factory index, skipping other game types.
    async fn wip_1006_game_at(&self, index: u64) -> Result<Option<Address>, ProposerError> {
        let entry = self
            .factory
            .gameAtIndex(U256::from(index))
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok((entry.gameType == MULTI_PROOF_GAME_TYPE).then_some(entry.proxy))
    }

    async fn read_resolution_status(
        &self,
        game: Address,
    ) -> Result<ResolutionStatus, ProposerError> {
        let result = self
            .game(game)
            .resolutionStatus()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let root_state = result
            .outcome
            .try_into()
            .map_err(|error: RootStateError| ProposerError::Contract(error.to_string()))?;
        let invalidation_reason = result
            .reason
            .try_into()
            .map_err(|error: InvalidationReasonError| ProposerError::Contract(error.to_string()))?;

        Ok(ResolutionStatus {
            resolvable: result.resolvable,
            root_state,
            invalidation_reason,
        })
    }

    async fn read_is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.anchor
            .isGameFinalized(game)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    /// Reads the credit `recipient` can unlock from `game`.
    async fn read_credit(&self, game: Address, recipient: Address) -> Result<U256, ProposerError> {
        self.game(game)
            .credit(recipient)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    /// Reads `recipient`'s pending `DelayedWETH` withdrawal for `game`.
    async fn read_pending_withdrawal(
        &self,
        game: Address,
        recipient: Address,
    ) -> Result<PendingWithdrawal, ProposerError> {
        let weth_address = self
            .game(game)
            .weth()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let weth = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone());

        let (pending, delay) = self
            .provider
            .multicall()
            .add(weth.withdrawals(game, recipient))
            .add(weth.delay())
            .aggregate()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        if pending.amount.is_zero() {
            return Ok(PendingWithdrawal::default());
        }
        let unlock_at = pending
            .timestamp
            .checked_add(delay)
            .ok_or_else(|| ProposerError::Contract("DelayedWETH unlock time overflows".into()))?;

        Ok(PendingWithdrawal {
            amount: pending.amount,
            unlock_at: u256_to_u64(unlock_at, "DelayedWETH unlock time")?,
        })
    }

    /// Sends `claimCredit(recipient)` and reports which phase of the two-phase flow ran.
    async fn send_claim_credit(
        &self,
        game: Address,
        recipient: Address,
    ) -> Result<ClaimSubmission, ProposerError> {
        // Read both phases up front so the submission can report what the call moved; the
        // receipt carries no amount of its own.
        let credit = self.read_credit(game, recipient).await?;
        let pending = if credit.is_zero() {
            self.read_pending_withdrawal(game, recipient).await?
        } else {
            PendingWithdrawal::default()
        };

        let pending_tx = self
            .game(game)
            .claimCredit(recipient)
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let tx_hash = *pending_tx.tx_hash();
        let receipt = pending_tx
            .get_receipt()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        Ok(if credit.is_zero() {
            ClaimSubmission {
                tx_hash,
                amount: pending.amount,
                withdrawn: true,
            }
        } else {
            ClaimSubmission {
                tx_hash,
                amount: credit,
                withdrawn: false,
            }
        })
    }
}

#[async_trait]
impl<P> BondManagerClient for AlloyProofSystemClient<P>
where
    P: Provider + WalletProvider + Clone + Send + Sync + 'static,
{
    fn proposer_address(&self) -> Address {
        self.provider.default_signer_address()
    }

    async fn game_count(&self) -> Result<u64, ProposerError> {
        let count = self
            .factory
            .gameCount()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        u256_to_u64(count, "gameCount")
    }

    async fn game_at(&self, index: u64) -> Result<Option<Address>, ProposerError> {
        self.wip_1006_game_at(index).await
    }

    async fn game_creator(&self, game: Address) -> Result<Address, ProposerError> {
        self.game(game)
            .gameCreator()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        self.read_resolution_status(game).await
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.read_is_game_finalized(game).await
    }

    async fn credit(&self, game: Address) -> Result<U256, ProposerError> {
        self.read_credit(game, self.proposer_address()).await
    }

    async fn pending_withdrawal(&self, game: Address) -> Result<PendingWithdrawal, ProposerError> {
        self.read_pending_withdrawal(game, self.proposer_address())
            .await
    }

    async fn latest_l1_timestamp(&self) -> Result<u64, ProposerError> {
        self.provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?
            .map(|block| block.header.timestamp())
            .ok_or_else(|| ProposerError::Contract("latest L1 block is unavailable".into()))
    }

    async fn claim_credit(&self, game: Address) -> Result<ClaimSubmission, ProposerError> {
        self.send_claim_credit(game, self.proposer_address()).await
    }
}

#[async_trait]
impl<P> ProposerClient for AlloyProofSystemClient<P>
where
    P: Provider + WalletProvider + Clone + Send + Sync + 'static,
{
    async fn anchor_parent(&self) -> Result<AnchorRef, ProposerError> {
        let (anchor_root, anchor_game) = tokio::try_join!(
            async {
                self.anchor
                    .getAnchorRoot()
                    .call()
                    .await
                    .map_err(|error| ProposerError::Contract(error.to_string()))
            },
            async {
                self.anchor
                    .anchorGame()
                    .call()
                    .await
                    .map_err(|error| ProposerError::Contract(error.to_string()))
            }
        )?;

        Ok(AnchorRef {
            registry: *self.anchor.address(),
            anchor_game: (anchor_game != Address::ZERO).then_some(anchor_game),
            l2_block_number: u256_to_u64(anchor_root.l2SequenceNumber, "getAnchorRoot")?,
        })
    }

    async fn games_for_transition(
        &self,
        anchor_game: Option<Address>,
        parent_candidates: &[Address],
        root_claim: B256,
        l2_block_number: u64,
    ) -> Result<Vec<TransitionGame>, ProposerError> {
        let game_count = self
            .factory
            .gameCount()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let game_count = u256_to_u64(game_count, "gameCount")?;
        if game_count == 0 {
            return Ok(Vec::new());
        }

        let candidates = self
            .transition_game_snapshot(anchor_game, game_count)
            .await?
            .iter()
            .filter(|game| {
                game.root_claim == root_claim
                    && game.data.l2_block_number == l2_block_number
                    && parent_candidates.contains(&game.data.parent_ref)
            })
            .cloned()
            .collect();

        Ok(select_transition_tips(candidates, parent_candidates))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        self.read_resolution_status(game).await
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        let pending = self
            .game(game)
            .resolve()
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        Ok(ResolveSubmission { tx_hash })
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.read_is_game_finalized(game).await
    }

    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        let pending = self
            .game(game)
            .closeGame()
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        Ok(CloseGameSubmission { tx_hash })
    }

    async fn latest_finalized_l1_block(&self) -> Result<BlockHash, ProposerError> {
        let block = self
            .provider
            .get_block(BlockId::finalized())
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let block = block.ok_or_else(|| ProposerError::FinalizedBlockNotFound)?;
        let hash = block.hash();
        Ok(hash)
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        retry_of: Option<Address>,
        proof: ProofData,
    ) -> Result<ProposalSubmission, ProposerError> {
        // `DisputeGameFactory.create` reverts unless `msg.value` matches the configured init
        // bond exactly, so it is read per submission rather than cached in configuration.
        let init_bond = self
            .factory
            .initBonds(MULTI_PROOF_GAME_TYPE)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let ProofData::Nitro {
            public_values,
            signature,
            public_key,
            ..
        } = proof
        else {
            return Err(ProposerError::InvalidProofResponse(
                "proposal creation requires a Nitro proof".into(),
            ));
        };
        let transition = TransitionPublicValues::abi_decode(&public_values)
            .map_err(|error| ProposerError::InvalidProofResponse(error.to_string()))?;
        let l1_origin_hash = transition.l1Head;
        let l1_origin = self
            .provider
            .get_block_by_hash(l1_origin_hash)
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?
            .ok_or_else(|| {
                ProposerError::InvalidProofResponse(format!(
                    "proof L1 head {} is unavailable",
                    l1_origin_hash
                ))
            })?;
        let l1_origin_number = l1_origin.header.number();
        let latest_l1_block = self
            .provider
            .get_block_number()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if latest_l1_block.saturating_sub(l1_origin_number) > MAX_CREATION_PROOF_AGE {
            return Err(ProposerError::StaleCreationProof {
                l1_origin_number,
                latest_l1_block,
            });
        }
        let creation_proof: Bytes = (
            self.domain_hash,
            proposal.parent_ref,
            U256::from(l1_origin_number),
            transition,
            signature,
            public_key,
        )
            .abi_encode_params()
            .into();
        let extra_data = proposal.commitment().extra_data(
            self.domain_hash,
            retry_of.unwrap_or(Address::ZERO),
            l1_origin_hash,
            l1_origin_number,
            ProofLane::TeeAttestation,
            creation_proof,
        );
        let pending = self
            .factory
            .create(MULTI_PROOF_GAME_TYPE, proposal.root_claim, extra_data)
            .value(init_bond)
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        let game_address = receipt
            .logs()
            .iter()
            .filter(|log| log.address() == *self.factory.address())
            .find_map(|log| {
                log.log_decode_validate::<IDisputeGameFactory::DisputeGameCreated>()
                    .ok()
                    .map(|decoded| decoded.inner.data)
            })
            .filter(|event| {
                event.gameType == MULTI_PROOF_GAME_TYPE && event.rootClaim == proposal.root_claim
            })
            .map(|event| event.disputeProxy)
            .ok_or_else(|| {
                ProposerError::Contract(format!(
                    "DisputeGameCreated event missing from proposal transaction {tx_hash}"
                ))
            })?;

        Ok(ProposalSubmission {
            tx_hash,
            game_address,
        })
    }
}

impl<P> AlloyProofSystemClient<P>
where
    P: Provider + Clone,
{
    /// Reads an L2 block number from a game contract.
    pub async fn game_l2_block_number(&self, game: Address) -> Result<u64, ProposerError> {
        let l2_block_number = self
            .game(game)
            .l2BlockNumber()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        u256_to_u64(l2_block_number, "l2BlockNumber")
    }
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, ProposerError> {
    value
        .try_into()
        .map_err(|_| ProposerError::Contract(format!("{field} overflows u64")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        index: u64,
        address: Address,
        parent_ref: Address,
        attempt: u64,
        retry_of: Address,
    ) -> ScannedTransitionGame {
        ScannedTransitionGame {
            factory_index: index,
            address,
            root_claim: B256::ZERO,
            data: ProposalExtraData {
                domain_hash: B256::ZERO,
                l2_block_number: 1,
                parent_ref,
                attempt,
                retry_of,
                l1_origin_hash: B256::ZERO,
                l1_origin_number: 0,
                creation_proof_lane: ProofLane::TeeAttestation,
                creation_proof: Bytes::from_static(&[1]),
            },
        }
    }

    #[test]
    fn selects_every_explicit_retry_lineage_tip() {
        let parent = Address::with_last_byte(1);
        let first = Address::with_last_byte(2);
        let parallel = Address::with_last_byte(3);
        let retry = Address::with_last_byte(4);
        let parallel_retry = Address::with_last_byte(5);

        let found = select_transition_tips(
            vec![
                candidate(14, parallel_retry, parent, 1, parallel),
                candidate(11, parallel, parent, 0, Address::ZERO),
                candidate(13, retry, parent, 1, first),
                candidate(10, first, parent, 0, Address::ZERO),
            ],
            &[parent],
        );

        assert_eq!(
            found,
            vec![
                TransitionGame {
                    address: retry,
                    parent_ref: parent,
                    attempt: 1,
                },
                TransitionGame {
                    address: parallel_retry,
                    parent_ref: parent,
                    attempt: 1,
                },
            ]
        );
    }
}
