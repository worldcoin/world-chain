use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, WalletProvider};
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use world_chain_proofs::{
    ANCHOR_PARENT_INDEX, IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory,
    IWorldChainProofSystemGame, InvalidationReasonError, ResolutionStatus, RootStateError,
};

use crate::{
    BondManagerClient, ParentRef, Proposal, ProposalSubmission, ProposerClient, ProposerError,
    types::{ClaimOutcome, CloseGameSubmission, ResolveSubmission},
};

/// Alloy-backed implementation of the proposer and bond-manager clients, wired to the stock
/// `DisputeGameFactory` and `AnchorStateRegistry`.
#[derive(Debug, Clone)]
pub struct AlloyProofSystemClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    game_type: u32,
    provider: P,
}

impl<P> AlloyProofSystemClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(
        provider: P,
        factory_address: Address,
        anchor_address: Address,
        game_type: u32,
    ) -> Self {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            anchor_address,
            provider.clone(),
        );

        Self {
            factory,
            anchor,
            game_type,
            provider,
        }
    }

    fn game_instance(
        &self,
        game: Address,
    ) -> IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance<P> {
        IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        )
    }

    /// Reads an L2 block number from a game contract.
    pub async fn game_l2_block_number(&self, game: Address) -> Result<u64, ProposerError> {
        let l2_block_number = self
            .game_instance(game)
            .l2BlockNumber()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        u256_to_u64(l2_block_number, "l2BlockNumber")
    }

    async fn latest_block_timestamp(&self) -> Result<u64, ProposerError> {
        let block = self
            .provider
            .get_block(BlockId::latest())
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?
            .ok_or_else(|| ProposerError::Contract("latest L1 block unavailable".to_string()))?;
        Ok(block.header.timestamp)
    }

    async fn game_at_index(&self, index: u64) -> Result<(u32, u64, Address), ProposerError> {
        let result = self
            .factory
            .gameAtIndex(U256::from(index))
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        Ok((result.gameType, result.timestamp, result.proxy))
    }

    /// Locates a game's factory creation index by binary-searching the (monotonically
    /// non-decreasing) creation timestamps, then scanning the equal-timestamp run.
    async fn find_game_index(&self, game: Address) -> Result<U256, ProposerError> {
        let created_at = self
            .game_instance(game)
            .createdAt()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let count: u64 = self
            .factory
            .gameCount()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
            .and_then(|count| u256_to_u64(count, "gameCount"))?;
        let (mut lo, mut hi) = (0u64, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (_, timestamp, _) = self.game_at_index(mid).await?;
            if timestamp < created_at {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        let mut index = lo;
        while index < count {
            let (_, timestamp, proxy) = self.game_at_index(index).await?;
            if timestamp > created_at {
                break;
            }
            if proxy == game {
                return Ok(U256::from(index));
            }
            index += 1;
        }

        Err(ProposerError::Contract(format!(
            "game {game} not found in DisputeGameFactory"
        )))
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
        let (game_type, _, proxy) = self.game_at_index(index).await?;
        Ok((game_type == self.game_type).then_some(proxy))
    }

    async fn game_proposer(&self, game: Address) -> Result<Address, ProposerError> {
        self.game_instance(game)
            .gameCreator()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        ProposerClient::resolution_status(self, game).await
    }

    async fn claim_credits(&self, game: Address) -> Result<ClaimOutcome, ProposerError> {
        let game_instance = self.game_instance(game);
        let proposer = self.provider.default_signer_address();

        let credit = game_instance
            .credit(proposer)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        // Phase 1: unassigned credit exists. `claimCredit` closes the game first, which
        // requires the registry's finality airgap to have elapsed.
        if credit > U256::ZERO {
            let finalized = ProposerClient::is_game_finalized(self, game).await?;
            if !finalized {
                return Ok(ClaimOutcome::NotReady);
            }

            let tx_hash = send_claim_credit(&game_instance, proposer).await?;
            return Ok(ClaimOutcome::Unlocked {
                tx_hash,
                amount: credit,
            });
        }

        // Phase 2: a DelayedWETH withdrawal is pending; finalize it once the delay elapses.
        let weth_address = game_instance
            .weth()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let weth = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone());
        let withdrawal = weth
            .withdrawals(game, proposer)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if withdrawal.amount == U256::ZERO {
            return Ok(ClaimOutcome::NoCredit);
        }

        let delay = weth
            .delay()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let now = self.latest_block_timestamp().await?;
        if withdrawal.timestamp + delay > U256::from(now) {
            return Ok(ClaimOutcome::NotReady);
        }

        let tx_hash = send_claim_credit(&game_instance, proposer).await?;
        Ok(ClaimOutcome::Claimed {
            tx_hash,
            amount: withdrawal.amount,
        })
    }
}

#[async_trait]
impl<P> ProposerClient for AlloyProofSystemClient<P>
where
    P: Provider + WalletProvider + Clone + Send + Sync + 'static,
{
    async fn anchor_parents(&self) -> Result<Vec<ParentRef>, ProposerError> {
        let anchor_root = self
            .anchor
            .getAnchorRoot()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let l2_block_number = u256_to_u64(anchor_root.l2SequenceNumber, "getAnchorRoot")?;
        let anchor_game = self
            .anchor
            .anchorGame()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let mut parents = Vec::with_capacity(2);
        // Children created before the anchor advanced reference the anchor game by factory
        // index; check it first so existing progress is discovered.
        if anchor_game != Address::ZERO {
            parents.push(ParentRef {
                address: anchor_game,
                l2_block_number,
                parent_index: self.find_game_index(anchor_game).await?,
            });
        }
        parents.push(ParentRef {
            address: *self.anchor.address(),
            l2_block_number,
            parent_index: ANCHOR_PARENT_INDEX,
        });
        Ok(parents)
    }

    async fn find_game(&self, proposal: &Proposal) -> Result<Option<Address>, ProposerError> {
        let result = self
            .factory
            .games(
                self.game_type,
                proposal.root_claim,
                proposal.extra_data().into(),
            )
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok((result.proxy != Address::ZERO).then_some(result.proxy))
    }

    async fn game_index(&self, game: Address) -> Result<U256, ProposerError> {
        self.find_game_index(game).await
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        let resolution_status_result = self
            .game_instance(game)
            .resolutionStatus()
            .call()
            .await
            .map_err(|err| ProposerError::Contract(err.to_string()))?;
        let resolvable = resolution_status_result.resolvable;
        let root_state = resolution_status_result
            .outcome
            .try_into()
            .map_err(|err: RootStateError| ProposerError::Contract(err.to_string()))?;
        let invalidation_reason = resolution_status_result
            .reason
            .try_into()
            .map_err(|err: InvalidationReasonError| ProposerError::Contract(err.to_string()))?;
        Ok(ResolutionStatus {
            resolvable,
            root_state,
            invalidation_reason,
        })
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.anchor
            .isGameFinalized(game)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        let pending = self
            .game_instance(game)
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

    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        let pending = self
            .game_instance(game)
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

    async fn proposer_bond(&self) -> Result<U256, ProposerError> {
        self.factory
            .initBonds(self.game_type)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError> {
        let pending = self
            .factory
            .create(
                self.game_type,
                proposal.root_claim,
                proposal.extra_data().into(),
            )
            .value(proposer_bond)
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
                event.gameType == self.game_type && event.rootClaim == proposal.root_claim
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

async fn send_claim_credit<P>(
    game: &IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance<P>,
    recipient: Address,
) -> Result<alloy_primitives::TxHash, ProposerError>
where
    P: Provider + Clone,
{
    let pending = game
        .claimCredit(recipient)
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

    Ok(tx_hash)
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, ProposerError> {
    value
        .try_into()
        .map_err(|_| ProposerError::Contract(format!("{field} overflows u64")))
}
