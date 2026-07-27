use crate::{
    error::ChallengerError,
    traits::{BondManagerClient, ChallengerClient, ResolutionManagerClient},
    types::{ChallengeSubmission, ClaimOutcome, GameMetadata, ResolveSubmission},
};
use alloy_primitives::{Address, TxHash, U256};
use alloy_provider::{Provider, WalletProvider};
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use world_chain_proofs::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IMultiProofGame,
    InvalidationReasonError, ResolutionStatus, RootState, RootStateError,
};

/// Alloy-backed implementation of the challenger contract clients, wired to the stock
/// `DisputeGameFactory`.
#[derive(Debug, Clone)]
pub struct AlloyChallengerClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    game_type: u32,
    provider: P,
}

impl<P> AlloyChallengerClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address, game_type: u32) -> Self {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );

        Self {
            factory,
            game_type,
            provider,
        }
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
    }

    /// Returns the game implementation registered for the World Chain game type. Bond and
    /// period values are constructor immutables, so they are read from the implementation
    /// rather than from a per-proposal clone.
    async fn game_impl(
        &self,
    ) -> Result<IMultiProofGame::IMultiProofGameInstance<P>, ChallengerError> {
        let address = self
            .factory
            .gameImpls(self.game_type)
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        if address.is_zero() {
            return Err(ChallengerError::Contract(format!(
                "no game implementation registered for game type {}",
                self.game_type
            )));
        }
        Ok(self.game(address))
    }

    async fn game_at_index(&self, index: u64) -> Result<(u32, u64, Address), ChallengerError> {
        let result = self
            .factory
            .gameAtIndex(U256::from(index))
            .block(BlockId::finalized())
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        Ok((result.gameType, result.timestamp, result.proxy))
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

    /// Returns the game at a factory index, filtered to the World Chain game type. The stock
    /// factory indexes every game type in one sequence.
    async fn read_game_address(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        let (game_type, _, proxy) = self.game_at_index(index).await?;
        Ok((game_type == self.game_type).then_some(proxy))
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

    /// Returns whether the registry the game was created against considers it finalized
    /// (resolved and past the finality airgap). `closeGame` — and therefore `claimCredit` —
    /// reverts before that point.
    async fn is_game_finalized(&self, address: Address) -> Result<bool, ChallengerError> {
        let game = self.game(address);
        let registry_address = game
            .anchorStateRegistry()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            registry_address,
            self.provider.clone(),
        )
        .isGameFinalized(address)
        .call()
        .await
        .map_err(|error| ChallengerError::Contract(error.to_string()))
    }

    async fn send_claim_credit(
        &self,
        game: &IMultiProofGame::IMultiProofGameInstance<P>,
        recipient: Address,
    ) -> Result<TxHash, ChallengerError> {
        let pending = game
            .claimCredit(recipient)
            .send()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ChallengerError::Revert(tx_hash));
        }
        Ok(tx_hash)
    }
}

#[async_trait]
impl<P> ChallengerClient for AlloyChallengerClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn challenger_bond(&self) -> Result<U256, ChallengerError> {
        self.game_impl()
            .await?
            .challengerBond()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))
    }

    async fn game_count(&self) -> Result<u64, ChallengerError> {
        self.read_game_count().await
    }

    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        self.read_game_address(index).await
    }

    async fn game_created_at(&self, index: u64) -> Result<u64, ChallengerError> {
        let (_, timestamp, _) = self.game_at_index(index).await?;
        Ok(timestamp)
    }

    async fn challenge_period(&self) -> Result<u64, ChallengerError> {
        self.game_impl()
            .await?
            .challengePeriod()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))
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
                game.l2SequenceNumber()
                    .call()
                    .await
                    .map_err(|error| ChallengerError::Contract(error.to_string()))
            }
        )?;

        Ok(GameMetadata {
            address,
            root_claim,
            l2_block_number: u256_to_u64(l2_block_number, "l2SequenceNumber")?,
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
        challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        let pending = self
            .game(address)
            .challenge()
            .value(challenger_bond)
            .send()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ChallengerError::Revert(tx_hash));
        }
        Ok(ChallengeSubmission { tx_hash })
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
            .get_receipt()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
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

    async fn claim_credits(&self, address: Address) -> Result<ClaimOutcome, ChallengerError> {
        let challenger = self.challenger_address();
        let game = self.game(address);

        let credit = game
            .credit(challenger)
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;

        // Phase 1: unassigned credit exists. `claimCredit` closes the game first, which
        // requires the registry's finality airgap to have elapsed.
        if credit > U256::ZERO {
            if !self.is_game_finalized(address).await? {
                return Ok(ClaimOutcome::NotReady);
            }

            let tx_hash = self.send_claim_credit(&game, challenger).await?;
            return Ok(ClaimOutcome::Unlocked {
                tx_hash,
                amount: credit,
            });
        }

        // Phase 2: a DelayedWETH withdrawal is pending; finalize it once the delay elapses.
        let weth_address = game
            .weth()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let weth = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone());
        let withdrawal = weth
            .withdrawals(address, challenger)
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        if withdrawal.amount == U256::ZERO {
            return Ok(ClaimOutcome::NoCredit);
        }

        let delay = weth
            .delay()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let now = U256::from(
            self.provider
                .get_block(BlockId::latest())
                .await
                .map_err(|error| ChallengerError::Rpc(error.to_string()))?
                .ok_or(ChallengerError::L1FinalizedBlockNotFound)?
                .header
                .timestamp,
        );
        if now < withdrawal.timestamp.saturating_add(delay) {
            return Ok(ClaimOutcome::NotReady);
        }

        let tx_hash = self.send_claim_credit(&game, challenger).await?;
        Ok(ClaimOutcome::Claimed {
            tx_hash,
            amount: withdrawal.amount,
        })
    }
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, ChallengerError> {
    value
        .try_into()
        .map_err(|_| ChallengerError::Contract(format!("{field} overflows u64")))
}
