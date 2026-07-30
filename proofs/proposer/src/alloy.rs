use alloy_consensus::BlockHeader;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::{Provider, WalletProvider};
use async_trait::async_trait;
use world_chain_proofs::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IMultiProofGame,
    InvalidationReasonError, MULTI_PROOF_GAME_TYPE, ResolutionStatus, RootStateError,
};

use crate::{
    AnchorRef, BondManagerClient, Proposal, ProposalSubmission, ProposerClient, ProposerError,
    types::{
        ClaimSubmission, CloseGameSubmission, PendingWithdrawal, ResolveSubmission, TransitionGame,
    },
};

/// Highest retry nonce probed when locating the live game for a transition.
///
/// Bounds the attempt walk so a corrupt or adversarial factory state cannot turn a single
/// canonical-line hop into an unbounded RPC loop.
const MAX_ATTEMPT_SCAN: u64 = 64;

/// Alloy-backed implementation of the proposer contract clients.
///
/// Binds the stock OP Stack `DisputeGameFactory` and `AnchorStateRegistry`; WIP-1006 games are
/// one game type among several on that factory, so every index-based read filters on
/// [`MULTI_PROOF_GAME_TYPE`].
#[derive(Debug, Clone)]
pub struct AlloyProofSystemClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    game_implementation: IMultiProofGame::IMultiProofGameInstance<P>,
    /// Domain hash of the registered game implementation, read once at construction.
    domain_hash: B256,
    /// Number of confirmations to require after sending a tx onchain.
    confirmations: u64,
    provider: P,
}

impl<P> AlloyProofSystemClient<P>
where
    P: Provider + Clone,
{
    /// Connects to the deployed proof system and reads the registered implementation's domain.
    ///
    /// Fails fast when the WIP-1006 game type has no implementation registered — either the
    /// factory address is wrong or the kill switch has cleared the implementation, and in both
    /// cases no proposal can succeed.
    pub async fn new(
        provider: P,
        factory_address: Address,
        anchor_address: Address,
        confirmations: u64,
    ) -> Result<Self, ProposerError> {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            anchor_address,
            provider.clone(),
        );

        let game_impl = factory
            .gameImpls(MULTI_PROOF_GAME_TYPE)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if game_impl == Address::ZERO {
            return Err(ProposerError::Contract(format!(
                "dispute-game factory {factory_address} has no implementation for game type {MULTI_PROOF_GAME_TYPE}"
            )));
        }
        let game_implementation =
            IMultiProofGame::IMultiProofGameInstance::new(game_impl, provider.clone());
        let domain_hash = game_implementation
            .domainHash()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok(Self {
            factory,
            anchor,
            game_implementation,
            domain_hash,
            confirmations,
            provider,
        })
    }

    /// Returns the domain hash of the registered game implementation.
    #[must_use]
    pub const fn domain_hash(&self) -> B256 {
        self.domain_hash
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
    }

    /// Resolves the WIP-1006 game at a factory index, skipping other game types.
    async fn wip_1006_game_at(&self, index: u64) -> Result<Option<Address>, ProposerError> {
        let entry = self
            .factory
            .gameAtIndex(U256::from(index))
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        Ok((entry.gameType == MULTI_PROOF_GAME_TYPE).then_some(entry.proxy))
    }

    async fn read_resolution_status(
        &self,
        game: Address,
    ) -> Result<ResolutionStatus, ProposerError> {
        let result = self
            .game(game)
            .resolutionStatus()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let root_state = result
            .outcome
            .try_into()
            .map_err(|error: RootStateError| ProposerError::Contract(error.to_string()))?;
        let invalidation_reason = result
            .reason
            .try_into()
            .map_err(|error: InvalidationReasonError| ProposerError::Contract(error.to_string()))?;

        Ok(ResolutionStatus {
            resolvable: result.resolvable,
            root_state,
            invalidation_reason,
        })
    }

    async fn read_is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.anchor
            .isGameFinalized(game)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    /// Reads the credit `recipient` can unlock from `game`.
    async fn read_credit(&self, game: Address, recipient: Address) -> Result<U256, ProposerError> {
        self.game(game)
            .credit(recipient)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    /// Reads `recipient`'s pending `DelayedWETH` withdrawal for `game`.
    async fn read_pending_withdrawal(
        &self,
        game: Address,
        recipient: Address,
    ) -> Result<PendingWithdrawal, ProposerError> {
        let weth_address = self
            .game(game)
            .weth()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let weth = IDelayedWETH::IDelayedWETHInstance::new(weth_address, self.provider.clone());

        let (pending, delay) = self
            .provider
            .multicall()
            .add(weth.withdrawals(game, recipient))
            .add(weth.delay())
            .aggregate()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        if pending.amount.is_zero() {
            return Ok(PendingWithdrawal::default());
        }
        let unlock_at = pending
            .timestamp
            .checked_add(delay)
            .ok_or_else(|| ProposerError::Contract("DelayedWETH unlock time overflows".into()))?;

        Ok(PendingWithdrawal {
            amount: pending.amount,
            unlock_at: u256_to_u64(unlock_at, "DelayedWETH unlock time")?,
        })
    }

    /// Sends `claimCredit(recipient)` and reports which phase of the two-phase flow ran.
    async fn send_claim_credit(
        &self,
        game: Address,
        recipient: Address,
    ) -> Result<ClaimSubmission, ProposerError> {
        // Read both phases up front so the submission can report what the call moved; the
        // receipt carries no amount of its own.
        let credit = self.read_credit(game, recipient).await?;
        let pending = if credit.is_zero() {
            self.read_pending_withdrawal(game, recipient).await?
        } else {
            PendingWithdrawal::default()
        };

        let pending_tx = self
            .game(game)
            .claimCredit(recipient)
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        let tx_hash = *pending_tx.tx_hash();
        let receipt = pending_tx
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
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
        self.wip_1006_game_at(index).await
    }

    async fn game_creator(&self, game: Address) -> Result<Address, ProposerError> {
        self.game(game)
            .gameCreator()
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        self.read_resolution_status(game).await
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.read_is_game_finalized(game).await
    }

    async fn credit(&self, game: Address) -> Result<U256, ProposerError> {
        self.read_credit(game, self.proposer_address()).await
    }

    async fn pending_withdrawal(&self, game: Address) -> Result<PendingWithdrawal, ProposerError> {
        self.read_pending_withdrawal(game, self.proposer_address())
            .await
    }

    async fn latest_l1_timestamp(&self) -> Result<u64, ProposerError> {
        self.provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?
            .map(|block| block.header.timestamp())
            .ok_or_else(|| ProposerError::Contract("latest L1 block is unavailable".into()))
    }

    async fn claim_credit(&self, game: Address) -> Result<ClaimSubmission, ProposerError> {
        self.send_claim_credit(game, self.proposer_address()).await
    }
}

#[async_trait]
impl<P> ProposerClient for AlloyProofSystemClient<P>
where
    P: Provider + WalletProvider + Clone + Send + Sync + 'static,
{
    async fn anchor_parent(&self) -> Result<AnchorRef, ProposerError> {
        let (anchor_root, canonical_parent) = self
            .provider
            .multicall()
            .add(self.anchor.getAnchorRoot())
            .add(self.game_implementation.canonicalAnchorParent())
            .aggregate()
            .await
            .map_err(|err| ProposerError::Contract(err.to_string()))?;

        Ok(AnchorRef {
            address: canonical_parent,
            l2_block_number: u256_to_u64(anchor_root.l2SequenceNumber, "getAnchorRoot")?,
        })
    }

    async fn game_for_transition(
        &self,
        parent_ref: Address,
        root_claim: B256,
        l2_block_number: u64,
    ) -> Result<Option<TransitionGame>, ProposerError> {
        let mut latest: Option<TransitionGame> = None;
        // Attempts are strictly sequential: attempt N can only be created once attempt N-1
        // exists, so the walk stops at the first gap.
        for attempt in 0..MAX_ATTEMPT_SCAN {
            let commitment = world_chain_proofs::ProposalCommitment {
                parent_ref,
                root_claim,
                l2_block_number,
                attempt,
            };
            let entry = self
                .factory
                .games(
                    MULTI_PROOF_GAME_TYPE,
                    root_claim,
                    commitment.extra_data(self.domain_hash),
                )
                .call()
                .await
                .map_err(|error| ProposerError::Contract(error.to_string()))?;
            if entry.proxy == Address::ZERO {
                break;
            }
            latest = Some(TransitionGame {
                address: entry.proxy,
                attempt,
            });
        }
        Ok(latest)
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        self.read_resolution_status(game).await
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        let pending = self
            .game(game)
            .resolve()
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        Ok(ResolveSubmission { tx_hash })
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        self.read_is_game_finalized(game).await
    }

    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        let pending = self
            .game(game)
            .closeGame()
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
            .get_receipt()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;
        if !receipt.status() {
            return Err(ProposerError::Revert(tx_hash));
        }

        Ok(CloseGameSubmission { tx_hash })
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
    ) -> Result<ProposalSubmission, ProposerError> {
        // `DisputeGameFactory.create` reverts unless `msg.value` matches the configured init
        // bond exactly, so it is read per submission rather than cached in configuration.
        let init_bond = self
            .factory
            .initBonds(MULTI_PROOF_GAME_TYPE)
            .call()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let extra_data = proposal.commitment().extra_data(self.domain_hash);
        let pending = self
            .factory
            .create(MULTI_PROOF_GAME_TYPE, proposal.root_claim, extra_data)
            .value(init_bond)
            .send()
            .await
            .map_err(|error| ProposerError::Contract(error.to_string()))?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .with_required_confirmations(self.confirmations)
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
                event.gameType == MULTI_PROOF_GAME_TYPE && event.rootClaim == proposal.root_claim
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
        let l2_block_number = self
            .game(game)
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
