use alloy_primitives::{Address, B256, U256};
use alloy_provider::Provider;
use async_trait::async_trait;

use crate::IMultiProofGame;

/// Immutable game context that defines the exact transition a proof worker must execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofGameContext {
    pub block_interval: u64,
    pub rollup_config_hash: B256,
    pub root_claim: B256,
    pub l2_block_number: u64,
    pub l1_head: B256,
}

impl ProofGameContext {
    /// Validates queued job metadata against the game and returns the exclusive range start.
    pub fn validated_start_block(
        self,
        game: Address,
        root_claim: B256,
        l2_block_number: u64,
        l1_head: B256,
        rollup_config_hash: B256,
    ) -> Result<u64, ProofGameContextError> {
        if self.root_claim != root_claim {
            return Err(ProofGameContextError::RootClaimMismatch {
                game,
                expected: self.root_claim,
                actual: root_claim,
            });
        }
        if self.l2_block_number != l2_block_number {
            return Err(ProofGameContextError::L2BlockNumberMismatch {
                game,
                expected: self.l2_block_number,
                actual: l2_block_number,
            });
        }
        if self.l1_head != l1_head {
            return Err(ProofGameContextError::L1HeadMismatch {
                game,
                expected: self.l1_head,
                actual: l1_head,
            });
        }
        if self.rollup_config_hash != rollup_config_hash {
            return Err(ProofGameContextError::RollupConfigHashMismatch {
                game,
                expected: self.rollup_config_hash,
                actual: rollup_config_hash,
            });
        }

        if self.block_interval == 0 {
            return Err(ProofGameContextError::InvalidBlockInterval {
                game,
                l2_block_number,
                block_interval: self.block_interval,
            });
        }

        l2_block_number.checked_sub(self.block_interval).ok_or(
            ProofGameContextError::InvalidBlockInterval {
                game,
                l2_block_number,
                block_interval: self.block_interval,
            },
        )
    }
}

/// Reads proof-critical metadata from the game associated with a leased job.
#[async_trait]
pub trait ProofGameProvider: Send + Sync + 'static {
    async fn proof_game_context(
        &self,
        game: Address,
    ) -> Result<ProofGameContext, ProofGameContextError>;
}

/// Alloy-backed proof-game reader using typed contract calls without a Multicall3 dependency.
#[derive(Clone, Debug)]
pub struct AlloyProofGameProvider<P> {
    provider: P,
}

impl<P> AlloyProofGameProvider<P> {
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl<P> ProofGameProvider for AlloyProofGameProvider<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn proof_game_context(
        &self,
        game_address: Address,
    ) -> Result<ProofGameContext, ProofGameContextError> {
        let game =
            IMultiProofGame::IMultiProofGameInstance::new(game_address, self.provider.clone());
        let block_interval_call = game.blockInterval();
        let rollup_config_hash_call = game.rollupConfigHash();
        let root_claim_call = game.rootClaim();
        let l2_block_number_call = game.l2SequenceNumber();
        let l1_head_call = game.l1Head();
        let (block_interval, rollup_config_hash, root_claim, l2_block_number, l1_head) =
            futures_util::try_join!(
                async { block_interval_call.call().await },
                async { rollup_config_hash_call.call().await },
                async { root_claim_call.call().await },
                async { l2_block_number_call.call().await },
                async { l1_head_call.call().await },
            )
            .map_err(|error| contract_error(game_address, error))?;

        Ok(ProofGameContext {
            block_interval: u256_to_u64(game_address, "blockInterval", block_interval)?,
            rollup_config_hash,
            root_claim,
            l2_block_number: u256_to_u64(game_address, "l2SequenceNumber", l2_block_number)?,
            l1_head,
        })
    }
}

fn contract_error(game: Address, error: impl ToString) -> ProofGameContextError {
    ProofGameContextError::Contract {
        game,
        error: error.to_string(),
    }
}

fn u256_to_u64(
    game: Address,
    field: &'static str,
    value: U256,
) -> Result<u64, ProofGameContextError> {
    value
        .try_into()
        .map_err(|_| ProofGameContextError::ValueOverflow { game, field, value })
}

#[derive(Debug, thiserror::Error)]
pub enum ProofGameContextError {
    #[error("failed to read proof game {game}: {error}")]
    Contract { game: Address, error: String },
    #[error("proof game {game} field {field} overflows u64: {value}")]
    ValueOverflow {
        game: Address,
        field: &'static str,
        value: U256,
    },
    #[error(
        "proof request root claim for game {game} does not match on-chain value: expected {expected}, got {actual}"
    )]
    RootClaimMismatch {
        game: Address,
        expected: B256,
        actual: B256,
    },
    #[error(
        "proof request L2 block for game {game} does not match on-chain value: expected {expected}, got {actual}"
    )]
    L2BlockNumberMismatch {
        game: Address,
        expected: u64,
        actual: u64,
    },
    #[error(
        "proof request L1 head for game {game} does not match on-chain value: expected {expected}, got {actual}"
    )]
    L1HeadMismatch {
        game: Address,
        expected: B256,
        actual: B256,
    },
    #[error(
        "worker rollup config for game {game} does not match on-chain value: expected {expected}, got {actual}"
    )]
    RollupConfigHashMismatch {
        game: Address,
        expected: B256,
        actual: B256,
    },
    #[error(
        "proof game {game} has invalid block interval {block_interval} for L2 block {l2_block_number}"
    )]
    InvalidBlockInterval {
        game: Address,
        l2_block_number: u64,
        block_interval: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;
    use alloy_provider::ProviderBuilder;
    use alloy_sol_types::SolValue;
    use alloy_transport::mock::Asserter;

    const GAME: Address = Address::repeat_byte(0x11);
    const ROOT: B256 = B256::repeat_byte(0x22);
    const L1_HEAD: B256 = B256::repeat_byte(0x33);
    const ROLLUP_CONFIG_HASH: B256 = B256::repeat_byte(0x44);

    fn context() -> ProofGameContext {
        ProofGameContext {
            block_interval: 450,
            rollup_config_hash: ROLLUP_CONFIG_HASH,
            root_claim: ROOT,
            l2_block_number: 1_000,
            l1_head: L1_HEAD,
        }
    }

    #[tokio::test]
    async fn reads_game_context_from_typed_contract_calls() {
        let asserter = Asserter::new();
        for response in [
            Bytes::from(U256::from(450).abi_encode()),
            Bytes::from(ROLLUP_CONFIG_HASH.abi_encode()),
            Bytes::from(ROOT.abi_encode()),
            Bytes::from(U256::from(1_000).abi_encode()),
            Bytes::from(L1_HEAD.abi_encode()),
        ] {
            asserter.push_success(&response);
        }
        let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());
        let game_provider = AlloyProofGameProvider::new(provider);

        assert_eq!(
            game_provider.proof_game_context(GAME).await.unwrap(),
            context()
        );
        assert!(asserter.read_q().is_empty());
    }

    #[test]
    fn validates_context_and_returns_range_start() {
        assert_eq!(
            context()
                .validated_start_block(GAME, ROOT, 1_000, L1_HEAD, ROLLUP_CONFIG_HASH)
                .unwrap(),
            550
        );
    }

    #[test]
    fn rejects_request_metadata_that_differs_from_game() {
        let error = context()
            .validated_start_block(
                GAME,
                B256::repeat_byte(0xff),
                1_000,
                L1_HEAD,
                ROLLUP_CONFIG_HASH,
            )
            .unwrap_err();

        assert!(matches!(
            error,
            ProofGameContextError::RootClaimMismatch { .. }
        ));
    }

    #[test]
    fn rejects_worker_rollup_config_that_differs_from_game() {
        let error = context()
            .validated_start_block(GAME, ROOT, 1_000, L1_HEAD, B256::repeat_byte(0xff))
            .unwrap_err();

        assert!(matches!(
            error,
            ProofGameContextError::RollupConfigHashMismatch { .. }
        ));
    }

    #[test]
    fn rejects_zero_block_interval() {
        let error = ProofGameContext {
            block_interval: 0,
            ..context()
        }
        .validated_start_block(GAME, ROOT, 1_000, L1_HEAD, ROLLUP_CONFIG_HASH)
        .unwrap_err();

        assert!(matches!(
            error,
            ProofGameContextError::InvalidBlockInterval { .. }
        ));
    }
}
