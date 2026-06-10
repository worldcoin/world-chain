use thiserror::Error;

/// Error returned to a defender by the `prover-service`.
#[derive(Error, Debug)]
pub enum ProofRequestError {}

/// Error returned to a prover worker by the `prover-service`.
#[derive(Error, Debug)]
pub enum ProofJobQueueError {}
