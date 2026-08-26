use crate::{
    error::ChallengerError,
    traits::{BondManagerClient, ChallengerClient, ResolutionManagerClient},
    types::{ChallengeSubmission, CloseGameSubmission, GameMetadata, ResolveSubmission},
};
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, WalletProvider};
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tracing::warn;
use world_chain_proof_protocol::{
    IAnchorStateRegistry, IDisputeGameFactory, IMultiProofGame, IWLDStakingVault,
    MULTI_PROOF_GAME_TYPE, ProposalStatus, ResolutionStatus, read_registered_bond_vault,
    read_registered_lineage_config,
};

/// Alloy-backed implementation of the challenger contract clients.
///
/// Binds the stock OP Stack `DisputeGameFactory`. Since WIP-1006 is one of several game types,
/// index-based reads filter on [`MULTI_PROOF_GAME_TYPE`].
#[derive(Debug, Clone)]
pub struct AlloyChallengerClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    bond_vault: IWLDStakingVault::IWLDStakingVaultInstance<P>,
    confirmations: u64,
    receipt_timeout: Duration,
    semaphore: Arc<Semaphore>,
    provider: P,
}

impl<P> AlloyChallengerClient<P>
where
    P: Provider + Clone,
{
    /// Creates an Alloy-backed contract client.
    pub async fn new(
        provider: P,
        factory_address: Address,
        confirmations: u64,
        receipt_timeout: Duration,
    ) -> Result<Self, ChallengerError> {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        read_registered_lineage_config(&provider, &factory).await?;
        let bond_vault = read_registered_bond_vault(&provider, &factory).await?;
        let init_bond = factory.initBonds(MULTI_PROOF_GAME_TYPE).call().await?;
        if !init_bond.is_zero() {
            return Err(ChallengerError::NonZeroFactoryBond(init_bond));
        }
        let vault = IWLDStakingVault::IWLDStakingVaultInstance::new(bond_vault, provider.clone());
        let configured_factory = vault.disputeGameFactory().call().await?;
        if configured_factory != factory_address {
            return Err(ChallengerError::VaultFactoryMismatch {
                vault: bond_vault,
                expected: factory_address,
                actual: configured_factory,
            });
        }
        let semaphore = Arc::new(Semaphore::new(1));
        Ok(Self {
            factory,
            bond_vault: vault,
            confirmations,
            receipt_timeout,
            semaphore,
            provider,
        })
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
    }

    /// Refreshes the managed challenger's reusable WLD balance metric.
    pub async fn refresh_vault_balance(&self)
    where
        P: WalletProvider,
    {
        self.refresh_vault_balance_for(self.provider.default_signer_address())
            .await;
    }

    /// Returns the singleton WLD vault discovered from the active implementation.
    #[must_use]
    pub fn bond_vault_address(&self) -> Address {
        *self.bond_vault.address()
    }

    async fn refresh_vault_balance_for(&self, account: Address) {
        match self.bond_vault.availableBalance(account).call().await {
            Ok(balance) => world_chain_proof_metrics::record_vault_balance(
                self.bond_vault_address(),
                account,
                "challenger",
                balance,
            ),
            Err(error) => warn!(%error, %account, "failed to fetch challenger WLD vault balance"),
        }
    }

    async fn read_game_count(&self) -> Result<u64, ChallengerError> {
        let count = self
            .factory
            .gameCount()
            .block(BlockId::finalized())
            .call()
            .await?;
        u256_to_u64(count)
    }

    /// Resolves the WIP-1006 game at a factory index, skipping other game types.
    async fn read_game_address(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        let entry = self
            .factory
            .gameAtIndex(U256::from(index))
            .block(BlockId::finalized())
            .call()
            .await?;

        Ok((entry.gameType == MULTI_PROOF_GAME_TYPE).then_some(entry.proxy))
    }

    async fn read_resolution_status(
        &self,
        address: Address,
    ) -> Result<ResolutionStatus, ChallengerError> {
        let result = self.game(address).resolutionStatus().call().await?;
        Ok(ResolutionStatus {
            resolvable: result.resolvable,
            outcome: result.outcome.try_into()?,
            invalidation_reason: result.reason.try_into()?,
        })
    }
}

#[async_trait]
impl<P> ChallengerClient for AlloyChallengerClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn game_count(&self) -> Result<u64, ChallengerError> {
        self.read_game_count().await
    }

    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        self.read_game_address(index).await
    }

    async fn game_metadata(&self, address: Address) -> Result<GameMetadata, ChallengerError> {
        let game = self.game(address);
        let (root_claim, l2_block_number) =
            tokio::try_join!(async { game.rootClaim().call().await }, async {
                game.l2SequenceNumber().call().await
            },)?;

        Ok(GameMetadata {
            address,
            root_claim,
            l2_block_number: u256_to_u64(l2_block_number)?,
        })
    }

    async fn proposal_status(&self, address: Address) -> Result<ProposalStatus, ChallengerError> {
        let claim = self.game(address).claimData().call().await?;
        claim.status.try_into().map_err(Into::into)
    }

    async fn challenge_deadline(&self, address: Address) -> Result<u64, ChallengerError> {
        Ok(self.game(address).challengeDeadline().call().await?)
    }

    async fn submit_challenge(
        &self,
        address: Address,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        let _permit = self.semaphore.acquire().await?;
        // Read the game-pinned amount so the submission and telemetry report the actual lock.
        let game = self.game(address);
        let challenger_bond = game.challengerBond().call().await?;

        let pending = game.challenge().send().await?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .with_timeout(Some(self.receipt_timeout))
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        if !receipt.status() {
            return Err(ChallengerError::Revert(tx_hash));
        }
        world_chain_proof_metrics::record_bond_locked("challenger", challenger_bond);
        self.refresh_vault_balance_for(receipt.from).await;
        Ok(ChallengeSubmission {
            tx_hash,
            bond: challenger_bond,
        })
    }
}

#[async_trait]
impl<P> ResolutionManagerClient for AlloyChallengerClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ChallengerError> {
        self.read_resolution_status(game).await
    }

    async fn resolve(&self, address: Address) -> Result<ResolveSubmission, ChallengerError> {
        let _permit = self.semaphore.acquire().await?;
        let pending = self.game(address).resolve().send().await?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .with_timeout(Some(self.receipt_timeout))
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;

        if !receipt.status() {
            return Err(ChallengerError::Revert(tx_hash));
        }
        Ok(ResolveSubmission { tx_hash })
    }
}

#[async_trait]
impl<P> BondManagerClient for AlloyChallengerClient<P>
where
    P: Provider + WalletProvider + Clone + Send + Sync + 'static,
{
    fn challenger_address(&self) -> Address {
        self.provider.default_signer_address()
    }

    async fn game_count(&self) -> Result<u64, ChallengerError> {
        self.read_game_count().await
    }

    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        self.read_game_address(index).await
    }

    async fn game_challenger(&self, address: Address) -> Result<Address, ChallengerError> {
        Ok(self.game(address).challenger().call().await?)
    }

    async fn is_game_finalized(&self, address: Address) -> Result<bool, ChallengerError> {
        let anchor_address = self.game(address).anchorStateRegistry().call().await?;
        let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            anchor_address,
            self.provider.clone(),
        );
        Ok(anchor.isGameFinalized(address).call().await?)
    }

    async fn is_game_settled(&self, address: Address) -> Result<bool, ChallengerError> {
        Ok(self.bond_vault.gameBonds(address).call().await?.settled)
    }

    async fn close_game(&self, address: Address) -> Result<CloseGameSubmission, ChallengerError> {
        let _permit = self.semaphore.acquire().await?;
        let pending_tx = self.game(address).closeGame().send().await?;
        let tx_hash = *pending_tx.tx_hash();
        let receipt = pending_tx
            .with_required_confirmations(self.confirmations)
            .with_timeout(Some(self.receipt_timeout))
            .get_receipt()
            .await?;
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;

        if !receipt.status() {
            return Err(ChallengerError::Revert(tx_hash));
        }

        self.refresh_vault_balance_for(receipt.from).await;

        Ok(CloseGameSubmission { tx_hash })
    }
}

fn u256_to_u64(value: U256) -> Result<u64, ChallengerError> {
    value.try_into().map_err(|_| ChallengerError::Overflow)
}
