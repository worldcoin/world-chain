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
use world_chain_proofs::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IMultiProofGame,
    InvalidationReasonError, MULTI_PROOF_GAME_TYPE, ResolutionStatus, RootState, RootStateError,
};

/// Alloy-backed implementation of the challenger contract clients.
///
/// Binds the stock OP Stack `DisputeGameFactory` and `AnchorStateRegistry`; WIP-1006 games are
/// one game type among several on that factory, so every index-based read filters on
/// [`MULTI_PROOF_GAME_TYPE`].
#[derive(Debug, Clone)]
pub struct AlloyChallengerClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    confirmations: u64,
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
        anchor_address: Address,
        confirmations: u64,
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
            confirmations,
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
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        u256_to_u64(count, "gameCount")
    }

    /// Resolves the WIP-1006 game at a factory index, skipping other game types.
    async fn read_game_address(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        let entry = self
            .factory
            .gameAtIndex(U256::from(index))
            .block(BlockId::finalized())
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;

        Ok((entry.gameType == MULTI_PROOF_GAME_TYPE).then_some(entry.proxy))
    }

    async fn read_resolution_status(
        &self,
        address: Address,
    ) -> Result<ResolutionStatus, ChallengerError> {
        let result = self
            .game(address)
            .resolutionStatus()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let root_state = result
            .outcome
            .try_into()
            .map_err(|error: RootStateError| ChallengerError::Contract(error.to_string()))?;
        let invalidation_reason =
            result
                .reason
                .try_into()
                .map_err(|error: InvalidationReasonError| {
                    ChallengerError::Contract(error.to_string())
                })?;

        Ok(ResolutionStatus {
            resolvable: result.resolvable,
            root_state,
            invalidation_reason,
        })
    }

    async fn read_credit(
        &self,
        address: Address,
        recipient: Address,
    ) -> Result<U256, ChallengerError> {
        self.game(address)
            .credit(recipient)
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))
    }

    async fn read_pending_withdrawal(
        &self,
        address: Address,
        recipient: Address,
    ) -> Result<PendingWithdrawal, ChallengerError> {
        let weth_address = self
            .game(address)
            .weth()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let weth = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone());

        let (pending, delay) = tokio::try_join!(
            async {
                weth.withdrawals(address, recipient)
                    .call()
                    .await
                    .map_err(|error| ChallengerError::Contract(error.to_string()))
            },
            async {
                weth.delay()
                    .call()
                    .await
                    .map_err(|error| ChallengerError::Contract(error.to_string()))
            }
        )?;

        if pending.amount.is_zero() {
            return Ok(PendingWithdrawal::default());
        }
        let unlock_at = pending.timestamp.checked_add(delay).ok_or_else(|| {
            ChallengerError::Contract("DelayedWETH unlock time overflows".to_string())
        })?;

        Ok(PendingWithdrawal {
            amount: pending.amount,
            unlock_at: u256_to_u64(unlock_at, "DelayedWETH unlock time")?,
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
        let (root_claim, l2_block_number) = tokio::try_join!(
            async {
                game.rootClaim()
                    .call()
                    .await
                    .map_err(|error| ChallengerError::Contract(error.to_string()))
            },
            async {
                game.l2BlockNumber()
                    .call()
                    .await
                    .map_err(|error| ChallengerError::Contract(error.to_string()))
            }
        )?;

        Ok(GameMetadata {
            address,
            root_claim,
            l2_block_number: u256_to_u64(l2_block_number, "l2BlockNumber")?,
        })
    }

    async fn root_state(&self, address: Address) -> Result<RootState, ChallengerError> {
        let raw = self
            .game(address)
            .state()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        raw.try_into().map_err(Into::into)
    }

    async fn challenge_deadline(&self, address: Address) -> Result<u64, ChallengerError> {
        self.game(address)
            .challengeDeadline()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))
    }

    async fn submit_challenge(
        &self,
        address: Address,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        // The bond is an immutable of whichever implementation this clone was created from, so
        // it is read per game: a re-registered implementation would otherwise make every
        // challenge revert with `IncorrectBondAmount`.
        let game = self.game(address);
        let challenger_bond = game
            .challengerBond()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;

        let pending = game
            .challenge()
            .value(challenger_bond)
            .send()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        crate::metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        if !receipt.status() {
            return Err(ChallengerError::Revert(tx_hash));
        }
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
        let pending = self
            .game(address)
            .resolve()
            .send()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        crate::metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
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
        self.game(address)
            .challenger()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))
    }

    async fn is_game_finalized(&self, address: Address) -> Result<bool, ChallengerError> {
        self.anchor
            .isGameFinalized(address)
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))
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
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?
            .map(|block| block.header.timestamp())
            .ok_or_else(|| ChallengerError::Contract("latest L1 block is unavailable".into()))
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

        let pending_tx = self
            .game(address)
            .claimCredit(recipient)
            .send()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let tx_hash = *pending_tx.tx_hash();
        let receipt = pending_tx
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        crate::metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        if !receipt.status() {
            return Err(ChallengerError::Revert(tx_hash));
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

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, ChallengerError> {
    value
        .try_into()
        .map_err(|_| ChallengerError::Contract(format!("{field} overflows u64")))
}
