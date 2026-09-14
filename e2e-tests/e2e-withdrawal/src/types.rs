use alloy_primitives::{B256, BlockNumber};

#[derive(Debug)]
pub enum StepOutcome {
    Initiated(InitiatedOutcome),
}

#[derive(Debug)]
pub struct InitiatedOutcome {
    pub tx_hash: B256,
    pub withdrawal_hash: B256,
    pub l2_block: BlockNumber,
}

#[derive(Debug, thiserror::Error)]
pub enum StepError {
    /// Generic eyre error.
    #[error("Generic error: {0}")]
    Generic(eyre::Report),
    /// Persistence error after transaction has already been sent onchain.
    #[error(
        "Persistence error after transaction has already been sent onchain. Tx hash: {transaction_hash}, source: {source}"
    )]
    PersistenceAfterTransaction {
        transaction_hash: B256,
        source: eyre::Report,
    },
}
