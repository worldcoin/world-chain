use alloy_primitives::{Address, B256, BlockNumber, TxHash};

/// Successful progress or expected waiting during the withdrawal workflow.
#[derive(Debug)]
pub enum StepOutcome {
    /// Initiation succeeded and its handoff was persisted.
    Initiated(InitiatedOutcome),
    /// Proving succeeded and its handoff was persisted.
    Proven(ProvenOutcome),
    /// A prerequisite is pending; no transaction was submitted by this invocation.
    Waiting(WaitingOutcome),
    /// Finalization was confirmed; `step` also completed handoff cleanup.
    Finalized(FinalizedOutcome),
}

/// Details of a successfully initiated withdrawal.
#[derive(Debug)]
pub struct InitiatedOutcome {
    /// L2 initiation transaction hash.
    pub tx_hash: B256,
    /// Withdrawal identifier emitted by the message passer.
    pub withdrawal_hash: B256,
    /// L2 block containing the initiation transaction.
    pub l2_block: BlockNumber,
}

/// Details of a successfully proven withdrawal.
#[derive(Debug)]
pub struct ProvenOutcome {
    /// L1 proving transaction hash.
    pub tx_hash: B256,
    /// Identifier of the proven withdrawal.
    pub withdrawal_hash: B256,
    /// L2 initiation block, not the covering game's block.
    pub withdrawal_l2_block: BlockNumber,
}

/// Whether finalization was already recorded or performed by this invocation.
#[derive(Debug)]
pub enum FinalizedOutcome {
    /// The portal already recorded the withdrawal as finalized.
    AlreadyFinalized {
        /// Identifier of the finalized withdrawal.
        withdrawal_hash: B256,
    },
    /// This invocation submitted and confirmed finalization.
    Finalized {
        /// Identifier of the finalized withdrawal.
        withdrawal_hash: B256,
        /// L1 finalization transaction hash.
        tx_hash: TxHash,
    },
}

impl FinalizedOutcome {
    /// Withdrawal hash associated with this finalize outcome.
    pub fn withdrawal_hash(&self) -> B256 {
        match self {
            Self::AlreadyFinalized { withdrawal_hash } => *withdrawal_hash,
            Self::Finalized {
                withdrawal_hash, ..
            } => *withdrawal_hash,
        }
    }
}

/// A withdrawal awaiting a normal on-chain prerequisite.
#[derive(Debug)]
pub struct WaitingOutcome {
    /// Identifier of the withdrawal awaiting progress.
    pub withdrawal_hash: B256,
    /// Stage that cannot proceed yet.
    pub stage: StepStage,
    /// Prerequisite that must be satisfied before retrying.
    pub waiting_reason: WaitingReason,
}

/// Stage of the withdrawal workflow associated with an outcome or failure.
#[derive(Debug)]
pub enum StepStage {
    /// Submit the withdrawal on L2 and persist its handoff.
    Init,
    /// Prove the withdrawal on L1 and persist its handoff.
    Prove,
    /// Confirm or perform finalization on L1.
    Finalize,
    /// Remove handoffs after confirmed finalization.
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

/// Expected conditions that can become satisfied as the chain progresses.
#[derive(Debug)]
pub enum WaitingReason {
    /// No suitable game was found covering the withdrawal's L2 block.
    CoveringGameUnavailable {
        /// L2 block that a covering game must include.
        withdrawal_l2_block: BlockNumber,
    },
    /// The portal's proof maturity delay has not elapsed.
    ProofMaturityNotElapsed {
        /// First eligible L1 block timestamp, in Unix seconds.
        eligible_at: u64,
    },
    /// The covering game is still in progress.
    GameUnresolved {
        /// Address of the unresolved game.
        game_address: Address,
    },
    /// The resolved game is still within the finality delay.
    GameFinalityDelay {
        /// Address of the game awaiting finality.
        game_address: Address,
    },
}

/// Conditions that block finalization rather than represent normal progress.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum GameBlockedReason {
    /// The system must be unpaused before finalization can proceed.
    #[error("system is paused")]
    SystemPaused,
    /// The game is absent from the registry's dispute game factory.
    #[error("game is not registered")]
    NotRegistered,
    /// The registry explicitly blacklisted the game.
    #[error("game is blacklisted")]
    Blacklisted,
    /// The game predates the registry's retirement cutoff.
    #[error("game is retired")]
    Retired,
    /// The game's type was not respected when it was created.
    #[error("game type was not respected when the game was created")]
    NotRespected,
    /// The game resolved against the withdrawal's supporting claim.
    #[error("game resolved in favor of the challenger")]
    ChallengerWon,
    /// The registry rejected the claim despite all known prerequisites passing.
    #[error("game claim is invalid despite satisfying known finalization prerequisites")]
    InvalidClaim,
}

/// A workflow failure; the variant alone does not make arbitrary retries safe.
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    /// Unclassified failure.
    #[error("Generic error: {0}")]
    Generic(#[from] eyre::Report),
    /// A required configuration value is invalid.
    #[error("Invalid configuration: {field}")]
    InvalidConfiguration {
        /// Configuration field name; never its secret value.
        field: &'static str,
    },
    /// Stored or observed state violates a workflow invariant.
    #[error("Invalid state during {stage}: {message}")]
    InvalidState {
        /// Stage that detected the invalid state.
        stage: StepStage,
        /// Description of the violated invariant.
        message: &'static str,
    },
    /// The supporting game cannot currently be used for finalization.
    #[error("Withdrawal {withdrawal_hash} is blocked by game {game_address}: {reason}")]
    GameBlocked {
        /// Identifier of the blocked withdrawal.
        withdrawal_hash: B256,
        /// Address of the supporting game.
        game_address: Address,
        /// Condition preventing finalization.
        reason: GameBlockedReason,
    },
    /// Persistence error after transaction has already been sent onchain.
    #[error(
        "Persistence error after transaction has already been sent onchain. Tx hash: {transaction_hash}, stage: {stage}, source: {source}"
    )]
    PersistenceAfterTransaction {
        /// Hash of the confirmed transaction whose handoff could not be saved.
        transaction_hash: B256,
        /// Stage that executed the transaction.
        stage: StepStage,
        /// Complete handoff retained so persistence retries need no remote reads.
        handoff: Box<serde_json::Value>,
        /// Underlying persistence failure.
        source: eyre::Report,
    },
    /// Handoff cleanup failed after the withdrawal was already finalized onchain.
    ///
    /// The next `step` tick should retry finalize (idempotent) and cleanup.
    #[error(
        "Cleanup error after withdrawal was finalized onchain. Withdrawal hash: {withdrawal_hash}, source: {source}"
    )]
    CleanupAfterFinalized {
        /// Identifier of the withdrawal already finalized on-chain.
        withdrawal_hash: B256,
        /// Underlying handoff deletion failure.
        source: eyre::Report,
    },
    /// Finalize tx succeeded but the portal read has not yet observed it.
    ///
    /// Usually caused by an RPC provider routing the confirmation read to a
    /// lagging node. The next tick should retry finalize (idempotent).
    #[error(
        "Portal did not yet show finalized withdrawal {withdrawal_hash} after tx {transaction_hash}"
    )]
    FinalizeNotYetVisible {
        /// Hash of the successful finalize transaction.
        transaction_hash: B256,
        /// Identifier of the withdrawal that should now be finalized.
        withdrawal_hash: B256,
    },
}

impl StepError {
    /// Return whether the error is retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::FinalizeNotYetVisible { .. }
                | Self::GameBlocked {
                    reason: GameBlockedReason::SystemPaused,
                    ..
                }
        )
    }
}
