use crate::{
    error::ChallengerError,
    traits::{BondManagerClient, ChallengerClient, ResolutionManagerClient},
    types::{ChallengeSubmission, GameMetadata, ResolveSubmission, WithdrawSubmission},
};
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, WalletProvider};
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use world_chain_proofs::{
    IDelayedWETH, IDisputeGameFactory, IWorldChainProofSystemGame, InvalidationReasonError,
    ResolutionStatus, RootState, RootStateError, WIP_1006_GAME_TYPE,
};

/// Alloy-backed implementation of the challenger contract clients.
#[derive(Debug, Clone)]
pub struct AlloyChallengerClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    provider: P,
}

impl<P> AlloyChallengerClient<P>
where
    P: Provider + Clone,
{
    /// Creates an Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address) -> Self {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );

        Self { factory, provider }
    }

    fn game(
        &self,
        address: Address,
    ) -> IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance<P> {
        IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            address,
            self.provider.clone(),
        )
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

    async fn read_game_address(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        let game = self
            .factory
            .gameAtIndex(U256::from(index))
            .block(BlockId::finalized())
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        Ok((game.gameType == WIP_1006_GAME_TYPE).then_some(game.proxy))
    }

    async fn game_implementation(&self) -> Result<Address, ChallengerError> {
        let implementation = self
            .factory
            .gameImpls(WIP_1006_GAME_TYPE)
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        if implementation == Address::ZERO {
            return Err(ChallengerError::Contract(
                "WIP-1006 game implementation is not registered".into(),
            ));
        }
        Ok(implementation)
    }

    async fn claimable_credit(
        &self,
        game_address: Address,
        recipient: Address,
    ) -> Result<U256, ChallengerError> {
        let game = self.game(game_address);
        let credit = game
            .credit(recipient)
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let weth_address = game
            .weth()
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?;
        let pending = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone())
            .withdrawals(game_address, recipient)
            .call()
            .await
            .map_err(|error| ChallengerError::Contract(error.to_string()))?
            .amount;
        credit
            .checked_add(pending)
            .ok_or_else(|| ChallengerError::Contract("claimable credit overflow".into()))
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
}

#[async_trait]
impl<P> ChallengerClient for AlloyChallengerClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn challenger_bond(&self) -> Result<U256, ChallengerError> {
        self.game(self.game_implementation().await?)
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

    async fn claimable(&self, address: Address) -> Result<U256, ChallengerError> {
        self.claimable_credit(address, self.challenger_address())
            .await
    }

    async fn withdraw(&self, address: Address) -> Result<WithdrawSubmission, ChallengerError> {
        let challenger = self.challenger_address();
        let game = self.game(address);
        let amount = self.claimable_credit(address, challenger).await?;
        let pending = game
            .claimCredit(challenger)
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

        Ok(WithdrawSubmission { tx_hash, amount })
    }
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, ChallengerError> {
    value
        .try_into()
        .map_err(|_| ChallengerError::Contract(format!("{field} overflows u64")))
}
