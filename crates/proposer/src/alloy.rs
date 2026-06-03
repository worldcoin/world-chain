use alloy_primitives::{Address, B256, U256};
use alloy_provider::{Provider, network::any::error};
use async_trait::async_trait;
use world_chain_proofs::{
    IWorldChainAnchorStateRegistry, IWorldChainProofSystemFactory, IWorldChainProofSystemGame,
    ProposalCommitment,
};

use crate::{ParentRef, ProofSystemClient, Proposal, ProposalSubmission, ProposerError};

/// Alloy-backed implementation of [`ProofSystemClient`].
#[derive(Debug, Clone)]
pub struct AlloyProofSystemClient<P> {
    factory: IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance<P>,
    anchor: IWorldChainAnchorStateRegistry::IWorldChainAnchorStateRegistryInstance<P>,
    provider: P,
    anchor_address: Address,
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
            anchor_address,
        }
    }
}

#[async_trait]
impl<P> ProofSystemClient for AlloyProofSystemClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
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
            address: anchor_parent_address(self.anchor_address, anchor_game),
            l2_block_number: u256_to_u64(l2_block_number, "currentL2BlockNumber")?,
        })
    }

    async fn proposal_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError> {
        self.factory
            .computeProposalKey(
                commitment.parent_ref,
                commitment.root_claim,
                U256::from(commitment.l2_block_number),
                commitment.intermediate_roots_hash,
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
                proposal.intermediate_roots_hash,
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
            return Err(ProposerError::Revert);
        }

        Ok(ProposalSubmission { tx_hash })
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
