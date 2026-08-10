use crate::{
    error::ChallengerError,
    traits::{BondManagerClient, ChallengerClient, ResolutionManagerClient},
    types::{
        ChallengeSubmission, ClaimSubmission, GameMetadata, PendingWithdrawal, ResolveSubmission,
    },
};
use alloy_consensus::BlockHeader;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, WalletProvider};
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use std::time::Duration;
use world_chain_proof_protocol::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IMultiProofGame,
    MULTI_PROOF_GAME_TYPE, ProposalStatus, ResolutionStatus,
};

/// Alloy-backed implementation of the challenger contract clients.
///
/// Binds the stock OP Stack `DisputeGameFactory`. Since WIP-1006 is one of several game types,
/// index-based reads filter on [`MULTI_PROOF_GAME_TYPE`].
#[derive(Debug, Clone)]
pub struct AlloyChallengerClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    confirmations: u64,
    receipt_timeout: Duration,
    provider: P,
}

impl<P> AlloyChallengerClient<P>
where
    P: Provider + Clone,
{
    /// Creates an Alloy-backed contract client.
    pub fn new(
        provider: P,
        factory_address: Address,
        confirmations: u64,
        receipt_timeout: Duration,
    ) -> Self {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );

        Self {
            factory,
            confirmations,
            receipt_timeout,
            provider,
        }
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
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

    async fn read_credit(
        &self,
        address: Address,
        recipient: Address,
    ) -> Result<U256, ChallengerError> {
        Ok(self.game(address).credit(recipient).call().await?)
    }

    async fn read_pending_withdrawal(
        &self,
        address: Address,
        recipient: Address,
    ) -> Result<PendingWithdrawal, ChallengerError> {
        let weth_address = self.game(address).weth().call().await?;
        let weth = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone());

        let (pending, delay) = tokio::try_join!(
            async { weth.withdrawals(address, recipient).call().await },
            async { weth.delay().call().await },
        )?;

        if pending.amount.is_zero() {
            return Ok(PendingWithdrawal::default());
        }
        let unlock_at = pending
            .timestamp
            .checked_add(delay)
            .ok_or(ChallengerError::Overflow)?;

        Ok(PendingWithdrawal {
            amount: pending.amount,
            unlock_at: u256_to_u64(unlock_at)?,
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
        // The bond is an immutable of whichever implementation this clone was created from, so
        // it is read per game: a re-registered implementation would otherwise make every
        // challenge revert with `IncorrectBondAmount`.
        let game = self.game(address);
        let challenger_bond = game.challengerBond().call().await?;

        let pending = game.challenge().value(challenger_bond).send().await?;
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
        world_chain_proof_metrics::record_bond_posted("challenger", challenger_bond);
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

    async fn credit(&self, address: Address) -> Result<U256, ChallengerError> {
        self.read_credit(address, self.challenger_address()).await
    }

    async fn pending_withdrawal(
        &self,
        address: Address,
    ) -> Result<PendingWithdrawal, ChallengerError> {
        self.read_pending_withdrawal(address, self.challenger_address())
            .await
    }

    async fn latest_l1_timestamp(&self) -> Result<u64, ChallengerError> {
        self.provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .map(|block| block.header.timestamp())
            .ok_or(ChallengerError::UnavailableLatestL1Block)
    }

    async fn claim_credit(&self, address: Address) -> Result<ClaimSubmission, ChallengerError> {
        let recipient = self.challenger_address();
        // Read both phases up front so the submission can report what the call moved; the
        // receipt carries no amount of its own.
        let credit = self.read_credit(address, recipient).await?;
        let pending = if credit.is_zero() {
            self.read_pending_withdrawal(address, recipient).await?
        } else {
            PendingWithdrawal::default()
        };

        let pending_tx = self.game(address).claimCredit(recipient).send().await?;
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

        Ok(if credit.is_zero() {
            world_chain_proof_metrics::record_bond_withdrawn("challenger", pending.amount);
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

fn u256_to_u64(value: U256) -> Result<u64, ChallengerError> {
    value.try_into().map_err(|_| ChallengerError::Overflow)
}
