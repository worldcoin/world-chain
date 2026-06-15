//! Generic proving worker for the World Chain defender stack.
//!
//! A [`ProofWorker`] leases proof jobs for a single backend lane from the `prover-service`,
//! proves them with a [`ProofJobBackend`], and submits the results back. The worker is
//! backend-agnostic: the SP1 and TEE workers are the same [`ProofWorker`] type instantiated
//! with different backends, and a defender process runs one instance per lane.
//!
//! ```text
//! PROVER-SERVICE --getNextProof(lane)--> PROOF-WORKER --prove--> BACKEND
//!        ^                                                          |
//!        +-------------------- submitProof <------------------------+
//! ```

mod backend;
mod worker;

pub use backend::ProofJobBackend;
pub use worker::{
    DEFAULT_MAX_CONCURRENT_JOBS, DEFAULT_POLL_INTERVAL, ProofWorker, ProofWorkerConfig,
};
