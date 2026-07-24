use alloy_primitives::{Address, B256, U256};
use alloy_provider::{Provider, WalletProvider};
use async_trait::async_trait;
use world_chain_proofs::{
    IWorldChainAnchorStateRegistry, IWorldChainProofSystemFactory, IWorldChainProofSystemGame,
    InvalidationReasonError, ProposalCommitment, ResolutionStatus, RootStateError,
};

use crate::{
    BondManagerClient, ParentRef, Proposal, ProposalSubmission, ProposerClient, ProposerError,
    types::{CloseGameSubmission, ResolveSubmission, WithdrawSubmission},
};

/// Alloy-backed implementation of [`ProofSystemClient`].
#[derive(Debug, Clone)]
pub struct AlloyProofSystemClient<P> {
    factory: IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance<P>,
    anchor: IWorldChainAnchorStateRegistry::IWorldChainAnchorStateRegistryInstance<P>,
    provider: P,
}

impl<P> AlloyProofSystemClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address, anchor_address: Address) -> Self {
        let factory = IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let anchor = IWorldChainAnchorStateRegistry::IWorldChainAnchorStateRegistryInstance::new(
            anchor_address,
            provider.clone(),
        );

        Self {
            factory,
            anchor,
            provider,
        }
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

    async fn game_at(&self, index: u64) -> Result<Address, ProposerError> {
        self.factory
            .gameAt(U256::from(index))
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn game_proposer(&self, game: Address) -> Result<Address, ProposerError> {
        IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        )
        .proposer()
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
        let l2_block_number = self
            .anchor
            .currentL2BlockNumber()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let anchor_game = self
            .anchor
            .anchorGame()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok(ParentRef {
            address: anchor_parent_address(*self.anchor.address(), anchor_game),
            l2_block_number: u256_to_u64(l2_block_number, "currentL2BlockNumber")?,
        })
    }

    async fn proposal_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError> {
        self.factory
            .computeProposalKey(
                commitment.parent_ref,
                commitment.root_claim,
                U256::from(commitment.l2_block_number),
            )
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn game_for_proposal_key(
        &self,
        proposal_key: B256,
    ) -> Result<Option<Address>, ProposerError> {
        let game = self
            .factory
            .games(proposal_key)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok((game != Address::ZERO).then_some(game))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
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
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
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
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
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

    async fn claimable(&self, game: Address) -> Result<U256, ProposerError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let proposer_address = self.provider.default_signer_address();
        let amount = game
            .claimable(proposer_address)
            .call()
            .await
            .map_err(|err| ProposerError::Contract(err.to_string()))?;

        Ok(amount)
    }

    async fn withdraw(&self, game: Address) -> Result<WithdrawSubmission, ProposerError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let proposer_address = self.provider.default_signer_address();
        let pending = game
            .withdraw(proposer_address)
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

        let amount = receipt
            .logs()
            .iter()
            .filter(|log| log.address() == *game.address())
            .find_map(|log| {
                log.log_decode_validate::<IWorldChainProofSystemGame::Withdrawn>()
                    .ok()
                    .map(|decoded| decoded.inner.data)
            })
            .filter(|event| event.recipient == proposer_address)
            .map(|event| event.amount)
            .ok_or_else(|| {
                ProposerError::Contract(format!(
                    "Withdrawn event missing from withdraw transaction {tx_hash}"
                ))
            })?;

        Ok(WithdrawSubmission { tx_hash, amount })
    }

    async fn proposer_bond(&self) -> Result<U256, ProposerError> {
        let proposer_bond = self
            .factory
            .proposerBond()
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
        let pending = self
            .factory
            .propose(
                proposal.parent_ref,
                proposal.root_claim,
                U256::from(proposal.l2_block_number),
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
                log.log_decode_validate::<IWorldChainProofSystemFactory::GameCreated>()
                    .ok()
                    .map(|decoded| decoded.inner.data)
            })
            .filter(|event| event.proposalKey == proposal.proposal_key)
            .map(|event| event.game)
            .ok_or_else(|| {
                ProposerError::Contract(format!(
                    "GameCreated event missing from proposal transaction {tx_hash}"
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
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let l2_block_number = game
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

fn anchor_parent_address(anchor_address: Address, anchor_game: Address) -> Address {
    if anchor_game == Address::ZERO {
        anchor_address
    } else {
        anchor_game
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, address};

    use super::anchor_parent_address;

    const ANCHOR: Address = address!("0000000000000000000000000000000000001006");
    const GAME: Address = address!("0000000000000000000000000000000000000001");

    #[test]
    fn anchor_parent_address_uses_registry_before_first_game() {
        assert_eq!(anchor_parent_address(ANCHOR, Address::ZERO), ANCHOR);
    }

    #[test]
    fn anchor_parent_address_uses_anchor_game_after_registry_advances() {
        assert_eq!(anchor_parent_address(ANCHOR, GAME), GAME);
    }
}
