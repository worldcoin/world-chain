use alloy_primitives::{Address, B256, U256};
use alloy_provider::{Provider, WalletProvider};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tracing::warn;
use world_chain_proof_protocol::{
    IAnchorStateRegistry, IDisputeGameFactory, IERC20StakingVault, IMultiProofGame, LineageAnchor,
    LineageError, LineageGame, LineageProvider, LineageTransition, MULTI_PROOF_GAME_TYPE,
    RegisteredLineageConfig, ResolutionStatus, read_game_for_transition, read_game_has_retry,
    read_lineage_anchor, read_lineage_resolution_status, read_registered_bond_vault,
    read_registered_lineage_config,
};

use crate::{
    BondManagerClient, Proposal, ProposalSubmission, ProposerClient, ProposerError,
    types::{CloseGameSubmission, ResolveSubmission},
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
    vault: IERC20StakingVault::IERC20StakingVaultInstance<P>,
    registered: RegisteredLineageConfig,
    /// Number of confirmations to require after sending a tx onchain.
    confirmations: u64,
    receipt_timeout: Duration,
    semaphore: Arc<Semaphore>,
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
        receipt_timeout: Duration,
    ) -> Result<Self, ProposerError> {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let registered = read_registered_lineage_config(&provider, &factory).await?;
        let bond_vault = read_registered_bond_vault(&provider, &factory).await?;
        let init_bond = factory.initBonds(MULTI_PROOF_GAME_TYPE).call().await?;
        if !init_bond.is_zero() {
            return Err(ProposerError::NonZeroFactoryBond(init_bond));
        }
        let vault =
            IERC20StakingVault::IERC20StakingVaultInstance::new(bond_vault, provider.clone());
        let configured_factory = vault.disputeGameFactory().call().await?;
        if configured_factory != factory_address {
            return Err(ProposerError::VaultFactoryMismatch {
                vault: bond_vault,
                expected: factory_address,
                actual: configured_factory,
            });
        }
        let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            registered.anchor_registry,
            provider.clone(),
        );
        let semaphore = Arc::new(Semaphore::new(1));

        Ok(Self {
            factory,
            anchor,
            vault,
            registered,
            confirmations,
            receipt_timeout,
            provider,
            semaphore,
        })
    }

    /// Returns the immutable configuration of the registered game implementation.
    #[must_use]
    pub const fn registered_lineage_config(&self) -> RegisteredLineageConfig {
        self.registered
    }

    /// Returns the singleton ERC-20 vault discovered from the active implementation.
    #[must_use]
    pub fn bond_vault_address(&self) -> Address {
        *self.vault.address()
    }

    /// Refreshes the managed proposer's reusable bond-token balance metric.
    pub async fn refresh_vault_balance(&self)
    where
        P: WalletProvider,
    {
        self.refresh_vault_balance_for(self.provider.default_signer_address())
            .await;
    }

    async fn refresh_vault_balance_for(&self, account: Address) {
        match self.vault.availableBalance(account).call().await {
            Ok(balance) => world_chain_proof_metrics::record_vault_balance(
                self.bond_vault_address(),
                account,
                "proposer",
                balance,
            ),
            Err(error) => warn!(%error, %account, "failed to fetch proposer ERC-20 vault balance"),
        }
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

    async fn read_is_game_claim_valid(&self, game: Address) -> Result<bool, ProposerError> {
        self.anchor
            .isGameClaimValid(game)
            .call()
            .await
            .map_err(Into::into)
    }

    async fn send_resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        let _permit = self.semaphore.acquire().await?;
        let pending = self.game(game).resolve().send().await?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .with_timeout(Some(self.receipt_timeout))
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        Ok(ResolveSubmission { tx_hash })
    }

    async fn send_close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        let _permit = self.semaphore.acquire().await?;
        let pending_tx = self.game(game).closeGame().send().await?;
        let tx_hash = *pending_tx.tx_hash();
        let receipt = pending_tx
            .with_required_confirmations(self.confirmations)
            .with_timeout(Some(self.receipt_timeout))
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }
        self.refresh_vault_balance_for(receipt.from).await;

        Ok(CloseGameSubmission { tx_hash })
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

    fn active_domain_hash(&self) -> B256 {
        self.registered.domain_hash
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

    async fn game_domain_hash(&self, game: Address) -> Result<B256, ProposerError> {
        self.game(game)
            .proposalDomainHash()
            .call()
            .await
            .map_err(Into::into)
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        self.read_resolution_status(game).await
    }

    async fn has_retry(&self, game: Address) -> Result<bool, ProposerError> {
        read_game_has_retry(&self.factory, &self.game(game))
            .await
            .map_err(Into::into)
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        self.send_resolve_game(game).await
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.read_is_game_finalized(game).await
    }

    async fn is_game_settled(&self, game: Address) -> Result<bool, ProposerError> {
        Ok(self.vault.gameBonds(game).call().await?.settled)
    }

    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        self.send_close_game(game).await
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

    async fn is_game_claim_valid(&self, game: Address) -> Result<bool, ProposerError> {
        self.read_is_game_claim_valid(game).await
    }

    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        self.send_close_game(game).await
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
    ) -> Result<ProposalSubmission, ProposerError> {
        let _permit = self.semaphore.acquire().await?;
        let extra_data = proposal
            .commitment()
            .extra_data(self.registered.domain_hash);
        let pending = self
            .factory
            .create(MULTI_PROOF_GAME_TYPE, proposal.root_claim, extra_data)
            .send()
            .await?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .with_timeout(Some(self.receipt_timeout))
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }
        let locked = receipt
            .logs()
            .iter()
            .filter(|log| log.address() == *self.vault.address())
            .find_map(|log| {
                log.log_decode_validate::<IERC20StakingVault::ProposerBondLocked>()
                    .ok()
                    .map(|decoded| decoded.inner.data.amount)
            })
            .ok_or(ProposerError::MissingProposerBondLockedEvent(tx_hash))?;
        world_chain_proof_metrics::record_bond_locked("proposer", locked);
        self.refresh_vault_balance_for(self.proposer_address())
            .await;

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
        let l2_block_number = self.game(game).l2SequenceNumber().call().await?;

        u256_to_u64(l2_block_number)
    }
}

fn u256_to_u64(value: U256) -> Result<u64, ProposerError> {
    value.try_into().map_err(|_| ProposerError::Overflow)
}
