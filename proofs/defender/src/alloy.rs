use crate::{
    error::DefenderError,
    traits::DefenderClient,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, Bytes, U256};
use alloy_provider::Provider;
use async_trait::async_trait;
use world_chain_proofs::{
    IAnchorStateRegistry, IDisputeGameFactory, IMultiProofGame, LineageAnchor, LineageError,
    LineageGame, LineageProvider, LineageTransition, PROOF_LANE_COUNT, RegisteredLineageConfig,
    ResolutionStatus, read_game_for_transition, read_lineage_anchor,
    read_lineage_resolution_status, read_registered_lineage_config,
};

/// Alloy-backed implementation of [`DefenderClient`].
///
/// Binds the stock OP Stack `DisputeGameFactory` and the anchor registry configured by its
/// registered WIP-1006 implementation.
#[derive(Debug, Clone)]
pub struct AlloyDefenderClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    registered: RegisteredLineageConfig,
    confirmations: u64,
    provider: P,
}

impl<P> AlloyDefenderClient<P>
where
    P: Provider + Clone,
{
    /// Connects to the registered WIP-1006 implementation and its anchor registry.
    pub async fn new(
        provider: P,
        factory_address: Address,
        confirmations: u64,
    ) -> Result<Self, DefenderError> {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let registered = read_registered_lineage_config(&provider, &factory).await?;
        let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            registered.anchor_registry,
            provider.clone(),
        );

        Ok(Self {
            factory,
            anchor,
            registered,
            confirmations,
            provider,
        })
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
    }
}

#[async_trait]
impl<P> LineageProvider for AlloyDefenderClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
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
impl<P> DefenderClient for AlloyDefenderClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
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

    async fn proof_bitmap(&self, address: Address) -> Result<u8, DefenderError> {
        self.game(address)
            .proofBitmap()
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))
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
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
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
