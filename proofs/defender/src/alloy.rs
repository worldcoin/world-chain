use crate::{
    error::DefenderError,
    traits::DefenderClient,
    types::{DefenderSubmission, GameMetadata, ResolveSubmission},
};
use alloy_primitives::{Address, Bytes, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use world_chain_proofs::{
    IDisputeGameFactory, IMultiProofGame, MULTI_PROOF_GAME_TYPE, PROOF_LANE_COUNT, ResolutionStatus,
};

/// Alloy-backed implementation of [`DefenderClient`].
///
/// Binds the stock OP Stack `DisputeGameFactory`; WIP-1006 games are one game type among
/// several on that factory, so every index-based read filters on [`MULTI_PROOF_GAME_TYPE`].
#[derive(Debug, Clone)]
pub struct AlloyDefenderClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    confirmations: u64,
    provider: P,
}

impl<P> AlloyDefenderClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address, confirmations: u64) -> Self {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );

        Self {
            factory,
            confirmations,
            provider,
        }
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
    }
}

#[async_trait]
impl<P> DefenderClient for AlloyDefenderClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn game_count(&self) -> Result<u64, DefenderError> {
        let count = self
            .factory
            .gameCount()
            .block(BlockId::finalized())
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))?;
        u256_to_u64(count, "gameCount")
    }

    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, DefenderError> {
        let entry = self
            .factory
            .gameAtIndex(U256::from(index))
            .block(BlockId::finalized())
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))?;

        Ok((entry.gameType == MULTI_PROOF_GAME_TYPE).then_some(entry.proxy))
    }

    async fn game_created_at(&self, index: u64) -> Result<u64, DefenderError> {
        self.factory
            .gameAtIndex(U256::from(index))
            .block(BlockId::finalized())
            .call()
            .await
            .map(|entry| entry.timestamp)
            .map_err(|error| DefenderError::Contract(error.to_string()))
    }

    async fn game_creator(&self, address: Address) -> Result<Address, DefenderError> {
        self.game(address)
            .gameCreator()
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))
    }

    async fn game_metadata(&self, address: Address) -> Result<GameMetadata, DefenderError> {
        let game = self.game(address);
        let (
            domain_hash,
            parent_ref,
            root_claim,
            l2_block_number,
            l1_origin_hash,
            l1_origin_number,
            challenge_deadline,
            proof_deadline,
            proof_threshold,
        ) = self
            .provider
            .multicall()
            .add(game.proposalDomainHash())
            .add(game.parentRef())
            .add(game.rootClaim())
            .add(game.l2BlockNumber())
            .add(game.l1OriginHash())
            .add(game.l1OriginNumber())
            .add(game.challengeDeadline())
            .add(game.proofDeadline())
            .add(game.PROOF_THRESHOLD())
            .aggregate()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))?;
        if proof_threshold == 0 || proof_threshold > PROOF_LANE_COUNT {
            return Err(DefenderError::Contract(format!(
                "invalid proof threshold {proof_threshold} for game {address}"
            )));
        }

        Ok(GameMetadata {
            address,
            domain_hash,
            parent_ref,
            root_claim,
            l2_block_number: u256_to_u64(l2_block_number, "l2BlockNumber")?,
            l1_origin_hash,
            l1_origin_number: u256_to_u64(l1_origin_number, "l1OriginNumber")?,
            challenge_deadline,
            proof_deadline,
            proof_threshold,
        })
    }

    async fn resolution_status(&self, address: Address) -> Result<ResolutionStatus, DefenderError> {
        let result = self
            .game(address)
            .resolutionStatus()
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))?;
        let root_state = result.outcome.try_into()?;
        let invalidation_reason = result.reason.try_into()?;

        Ok(ResolutionStatus {
            resolvable: result.resolvable,
            root_state,
            invalidation_reason,
        })
    }

    async fn proof_bitmap(&self, address: Address) -> Result<u8, DefenderError> {
        self.game(address)
            .proofBitmap()
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, DefenderError> {
        let pending = self
            .game(game)
            .resolve()
            .send()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        if !receipt.status() {
            return Err(DefenderError::Revert(tx_hash));
        }
        Ok(ResolveSubmission { tx_hash })
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
        let pending = self
            .game(game)
            .submitProofLane(lane, proof)
            .send()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        if !receipt.status() {
            return Err(DefenderError::Revert(tx_hash));
        }
        Ok(DefenderSubmission { tx_hash })
    }
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, DefenderError> {
    value
        .try_into()
        .map_err(|_| DefenderError::Contract(format!("{field} overflows u64")))
}
