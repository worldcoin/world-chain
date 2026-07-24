use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{Provider, WalletProvider};
use alloy_sol_types::SolValue;
use async_trait::async_trait;
use world_chain_proofs::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IWorldChainProofSystemGame,
    InvalidationReasonError, ProposalCommitment, ResolutionStatus, RootStateError,
    WIP_1006_GAME_TYPE,
};

use crate::{
    BondManagerClient, ParentRef, Proposal, ProposalSubmission, ProposerClient, ProposerError,
    types::{CloseGameSubmission, ResolveSubmission, WithdrawSubmission},
};

/// Alloy-backed implementation of [`ProofSystemClient`].
#[derive(Debug, Clone)]
pub struct AlloyProofSystemClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    provider: P,
}

impl<P> AlloyProofSystemClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address, anchor_address: Address) -> Self {
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
            provider,
        }
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

    async fn game_implementation(&self) -> Result<Address, ProposerError> {
        let implementation = self
            .factory
            .gameImpls(WIP_1006_GAME_TYPE)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if implementation == Address::ZERO {
            return Err(ProposerError::Contract(
                "WIP-1006 game implementation is not registered".into(),
            ));
        }
        Ok(implementation)
    }

    async fn domain_hash(&self) -> Result<B256, ProposerError> {
        self.game(self.game_implementation().await?)
            .domainHash()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn lookup_game(
        &self,
        commitment: ProposalCommitment,
        domain_hash: B256,
        attempt: u64,
    ) -> Result<Address, ProposerError> {
        let extra_data = proposal_extra_data(commitment, domain_hash, attempt);
        self.factory
            .games(WIP_1006_GAME_TYPE, commitment.root_claim, extra_data)
            .call()
            .await
            .map(|result| result.proxy)
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn next_attempt(
        &self,
        commitment: ProposalCommitment,
        domain_hash: B256,
    ) -> Result<(u64, Option<Address>), ProposerError> {
        let mut attempt = 0_u64;
        let mut latest_respected_game = None;
        loop {
            let game = self.lookup_game(commitment, domain_hash, attempt).await?;
            if game == Address::ZERO {
                return Ok((attempt, latest_respected_game));
            }
            if self
                .game(game)
                .wasRespectedGameTypeWhenCreated()
                .call()
                .await
                .map_err(|error| ProposerError::Contract(error.to_string()))?
            {
                latest_respected_game = Some(game);
            } else {
                latest_respected_game = None;
            }
            attempt = attempt
                .checked_add(1)
                .ok_or_else(|| ProposerError::Contract("proposal attempt overflow".into()))?;
        }
    }

    async fn claimable_credit(
        &self,
        game_address: Address,
        recipient: Address,
    ) -> Result<U256, ProposerError> {
        let game = self.game(game_address);
        let credit = game
            .credit(recipient)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let weth_address = game
            .weth()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let pending = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone())
            .withdrawals(game_address, recipient)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?
            .amount;
        credit
            .checked_add(pending)
            .ok_or_else(|| ProposerError::Contract("claimable credit overflow".into()))
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
        let game = self
            .factory
            .gameAtIndex(U256::from(index))
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        Ok((game.gameType == WIP_1006_GAME_TYPE).then_some(game.proxy))
    }

    async fn game_proposer(&self, game: Address) -> Result<Address, ProposerError> {
        self.game(game)
            .gameCreator()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        ProposerClient::resolution_status(self, game).await
    }

    async fn claimable(&self, game: Address) -> Result<U256, ProposerError> {
        ProposerClient::claimable(self, game).await
    }

    async fn withdraw(&self, game: Address) -> Result<WithdrawSubmission, ProposerError> {
        ProposerClient::withdraw(self, game).await
    }
}

#[async_trait]
impl<P> ProposerClient for AlloyProofSystemClient<P>
where
    P: Provider + WalletProvider + Clone + Send + Sync + 'static,
{
    async fn anchor_parent(&self) -> Result<ParentRef, ProposerError> {
        let anchor_root = self
            .anchor
            .getAnchorRoot()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        Ok(ParentRef {
            address: *self.anchor.address(),
            l2_block_number: u256_to_u64(anchor_root.l2SequenceNumber, "l2SequenceNumber")?,
        })
    }

    async fn transition_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError> {
        let domain_hash = self.domain_hash().await?;
        Ok(commitment.transition_key(domain_hash))
    }

    async fn game_for_proposal(
        &self,
        commitment: ProposalCommitment,
    ) -> Result<Option<Address>, ProposerError> {
        let domain_hash = self.domain_hash().await?;
        let (_, latest_respected_game) = self.next_attempt(commitment, domain_hash).await?;
        Ok(latest_respected_game)
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        let game = self.game(game);
        let resolution_status_result = game
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
        let resolution_status = ResolutionStatus {
            resolvable,
            root_state,
            invalidation_reason,
        };
        Ok(resolution_status)
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        let game = self.game(game);
        let pending = game
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
        let game = self.game(game);
        let pending = game
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

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.anchor
            .isGameFinalized(game)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn claimable(&self, game: Address) -> Result<U256, ProposerError> {
        self.claimable_credit(game, self.provider.default_signer_address())
            .await
    }

    async fn withdraw(&self, game_address: Address) -> Result<WithdrawSubmission, ProposerError> {
        let game = self.game(game_address);
        let proposer_address = self.provider.default_signer_address();
        let amount = self
            .claimable_credit(game_address, proposer_address)
            .await?;
        let pending = game
            .claimCredit(proposer_address)
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

        Ok(WithdrawSubmission { tx_hash, amount })
    }

    async fn proposer_bond(&self) -> Result<U256, ProposerError> {
        let proposer_bond = self
            .factory
            .initBonds(WIP_1006_GAME_TYPE)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok(proposer_bond)
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError> {
        let domain_hash = self.domain_hash().await?;
        let (attempt, _) = self
            .next_attempt(proposal.commitment(), domain_hash)
            .await?;
        let extra_data = proposal_extra_data(proposal.commitment(), domain_hash, attempt);
        let pending = self
            .factory
            .create(WIP_1006_GAME_TYPE, proposal.root_claim, extra_data)
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
                event.gameType == WIP_1006_GAME_TYPE && event.rootClaim == proposal.root_claim
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
        let game = self.game(game);
        let l2_block_number = game
            .l2SequenceNumber()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        u256_to_u64(l2_block_number, "l2SequenceNumber")
    }
}

fn proposal_extra_data(commitment: ProposalCommitment, domain_hash: B256, attempt: u64) -> Bytes {
    Bytes::from(
        (
            domain_hash,
            U256::from(commitment.l2_block_number),
            commitment.parent_ref,
            U256::from(attempt),
        )
            .abi_encode(),
    )
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, ProposerError> {
    value
        .try_into()
        .map_err(|_| ProposerError::Contract(format!("{field} overflows u64")))
}
