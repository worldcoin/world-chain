use alloy_primitives::{Address, B256, BlockNumber, TxHash};

#[derive(Debug)]
pub enum StepOutcome {
    Initiated(InitiatedOutcome),
    Proven(ProvenOutcome),
    Waiting(WaitingOutcome),
    Finalized(FinalizedOutcome),
}

#[derive(Debug)]
pub struct InitiatedOutcome {
    pub tx_hash: B256,
    pub withdrawal_hash: B256,
    pub l2_block: BlockNumber,
}

#[derive(Debug)]
pub struct ProvenOutcome {
    pub tx_hash: B256,
    pub withdrawal_hash: B256,
    pub l2_block: BlockNumber,
}

#[derive(Debug)]
pub enum FinalizedOutcome {
    AlreadyFinalized {
        withdrawal_hash: B256,
    },
    Finalized {
        witdrawal_hash: B256,
        tx_hash: TxHash,
    },
}

impl FinalizedOutcome {
    /// Withdrawal hash associated with this finalize outcome.
    pub fn withdrawal_hash(&self) -> B256 {
        match self {
            Self::AlreadyFinalized { withdrawal_hash } => *withdrawal_hash,
            Self::Finalized { witdrawal_hash, .. } => *witdrawal_hash,
        }
    }
}
#[derive(Debug)]
pub struct WaitingOutcome {
    pub withdrawal_hash: B256,
    pub stage: StepStage,
    pub waiting_reason: WaitingReason,
}

#[derive(Debug)]
pub enum StepStage {
    Init,
    Prove,
    Finalize,
    Cleanup,
}

impl std::fmt::Display for StepStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            StepStage::Init => "init",
            StepStage::Prove => "prove",
            StepStage::Finalize => "finalize",
            StepStage::Cleanup => "cleanup",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug)]
pub enum WaitingReason {
    CoveringGameUnavailable { withdrawal_l2_block: BlockNumber },
    ProofMaturityNotElapsed { eligible_at: u64 },
    GameClaimIsNotValid { game_address: Address },
}

#[derive(Debug, thiserror::Error)]
pub enum StepError {
    /// Generic eyre error.
    #[error("Generic error: {0}")]
    Generic(eyre::Report),
    /// Persistence error after transaction has already been sent onchain.
    #[error(
        "Persistence error after transaction has already been sent onchain. Tx hash: {transaction_hash}, stage: {stage}, source: {source}"
    )]
    PersistenceAfterTransaction {
        transaction_hash: B256,
        stage: StepStage,
        source: eyre::Report,
    },
    /// Handoff cleanup failed after the withdrawal was already finalized onchain.
    ///
    /// The next `step` tick should retry finalize (idempotent) and cleanup.
    #[error(
        "Cleanup error after withdrawal was finalized onchain. Withdrawal hash: {withdrawal_hash}, source: {source}"
    )]
    CleanupAfterFinalized {
        withdrawal_hash: B256,
        source: eyre::Report,
    },
}
