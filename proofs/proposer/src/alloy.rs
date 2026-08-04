use alloy_consensus::BlockHeader;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, WalletProvider};
use async_trait::async_trait;
use world_chain_proofs::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IMultiProofGame, LineageAnchor,
    LineageError, LineageGame, LineageProvider, LineageTransition, MULTI_PROOF_GAME_TYPE,
    RegisteredLineageConfig, ResolutionStatus, read_game_for_transition, read_lineage_anchor,
    read_lineage_resolution_status, read_registered_lineage_config,
};

use crate::{
    BondManagerClient, Proposal, ProposalSubmission, ProposerClient, ProposerError,
    types::{ClaimSubmission, CloseGameSubmission, PendingWithdrawal, ResolveSubmission},
};

/// Alloy-backed implementation of the proposer contract clients.
///
/// Binds the stock OP Stack `DisputeGameFactory` and `AnchorStateRegistry`; WIP-1006 games are
/// one game type among several on that factory, so every index-based read filters on
/// [`MULTI_PROOF_GAME_TYPE`].
#[derive(Debug, Clone)]
pub struct AlloyProofSystemClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    registered: RegisteredLineageConfig,
    /// Number of confirmations to require after sending a tx onchain.
    confirmations: u64,
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
        confirmations: u64,
    ) -> Result<Self, ProposerError> {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let registered = read_registered_lineage_config(&provider, &factory).await?;
        let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            registered.anchor_registry,
            provider.clone(),
        );

        Ok(Self {
            factory,
            anchor,
            registered,
            confirmations,
            provider,
        })
    }

    /// Returns the immutable configuration of the registered game implementation.
    #[must_use]
    pub const fn registered_lineage_config(&self) -> RegisteredLineageConfig {
        self.registered
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
    }

    /// Resolves the WIP-1006 game at a factory index, skipping other game types.
    async fn wip_1006_game_at(&self, index: u64) -> Result<Option<Address>, ProposerError> {
        let entry = self.factory.gameAtIndex(U256::from(index)).call().await?;

        Ok((entry.gameType == MULTI_PROOF_GAME_TYPE).then_some(entry.proxy))
    }

    async fn read_resolution_status(
        &self,
        game: Address,
    ) -> Result<ResolutionStatus, ProposerError> {
        read_lineage_resolution_status(&self.game(game))
            .await
            .map_err(Into::into)
    }

    async fn read_is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.anchor
            .isGameFinalized(game)
            .call()
            .await
            .map_err(Into::into)
    }

    async fn send_resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        let pending = self.game(game).resolve().send().await?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        Ok(ResolveSubmission { tx_hash })
    }

    /// Reads the credit `recipient` can unlock from `game`.
    async fn read_credit(&self, game: Address, recipient: Address) -> Result<U256, ProposerError> {
        self.game(game)
            .credit(recipient)
            .call()
            .await
            .map_err(Into::into)
    }

    /// Reads `recipient`'s pending `DelayedWETH` withdrawal for `game`.
    async fn read_pending_withdrawal(
        &self,
        game: Address,
        recipient: Address,
    ) -> Result<PendingWithdrawal, ProposerError> {
        let weth_address = self.game(game).weth().call().await?;
        let weth = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone());

        let (pending, delay) = self
            .provider
            .multicall()
            .add(weth.withdrawals(game, recipient))
            .add(weth.delay())
            .aggregate()
            .await?;

        if pending.amount.is_zero() {
            return Ok(PendingWithdrawal::default());
        }
        let unlock_at = pending
            .timestamp
            .checked_add(delay)
            .ok_or(ProposerError::Overflow)?;

        Ok(PendingWithdrawal {
            amount: pending.amount,
            unlock_at: u256_to_u64(unlock_at)?,
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

        let pending_tx = self.game(game).claimCredit(recipient).send().await?;
        let tx_hash = *pending_tx.tx_hash();
        let receipt = pending_tx
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
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
        let count = self.factory.gameCount().call().await?;
        u256_to_u64(count)
    }

    async fn game_at(&self, index: u64) -> Result<Option<Address>, ProposerError> {
        self.wip_1006_game_at(index).await
    }

    async fn game_creator(&self, game: Address) -> Result<Address, ProposerError> {
        self.game(game)
            .gameCreator()
            .call()
            .await
            .map_err(Into::into)
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        self.read_resolution_status(game).await
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        self.send_resolve_game(game).await
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
            .await?
            .map(|block| block.header.timestamp())
            .ok_or(ProposerError::UnavailableLatestL1Block)
    }

    async fn claim_credit(&self, game: Address) -> Result<ClaimSubmission, ProposerError> {
        self.send_claim_credit(game, self.proposer_address()).await
    }
}

#[async_trait]
impl<P> LineageProvider for AlloyProofSystemClient<P>
where
    P: Provider + WalletProvider + Clone + Send + Sync + 'static,
{
    fn lineage_block_interval(&self) -> u64 {
        self.registered.block_interval
    }

    async fn lineage_anchor(&self) -> Result<LineageAnchor, LineageError> {
        read_lineage_anchor(&self.provider, &self.anchor).await
    }

    async fn game_for_transition(
        &self,
        transition: LineageTransition,
    ) -> Result<Option<LineageGame>, LineageError> {
        read_game_for_transition(&self.factory, self.registered.domain_hash, transition).await
    }

    async fn lineage_resolution_status(
        &self,
        game: Address,
    ) -> Result<ResolutionStatus, LineageError> {
        read_lineage_resolution_status(&self.game(game)).await
    }
}

#[async_trait]
impl<P> ProposerClient for AlloyProofSystemClient<P>
where
    P: Provider + WalletProvider + Clone + Send + Sync + 'static,
{
    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        self.send_resolve_game(game).await
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.read_is_game_finalized(game).await
    }

    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        let pending = self.game(game).closeGame().send().await?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        Ok(CloseGameSubmission { tx_hash })
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
    ) -> Result<ProposalSubmission, ProposerError> {
        // `DisputeGameFactory.create` reverts unless `msg.value` matches the configured init
        // bond exactly, so it is read per submission rather than cached in configuration.
        let init_bond = self.factory.initBonds(MULTI_PROOF_GAME_TYPE).call().await?;

        let extra_data = proposal
            .commitment()
            .extra_data(self.registered.domain_hash);
        let pending = self
            .factory
            .create(MULTI_PROOF_GAME_TYPE, proposal.root_claim, extra_data)
            .value(init_bond)
            .send()
            .await?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
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
            .ok_or(ProposerError::MissingProposalEvent(tx_hash))?;

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
        let l2_block_number = self.game(game).l2BlockNumber().call().await?;

        u256_to_u64(l2_block_number)
    }
}

fn u256_to_u64(value: U256) -> Result<u64, ProposerError> {
    value.try_into().map_err(|_| ProposerError::Overflow)
}
