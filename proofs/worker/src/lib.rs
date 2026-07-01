//! Generic proving worker for the World Chain defender stack.
//!
//! A [`ProofWorker`] polls the `prover-service` for claimed proof jobs on a single backend
//! lane, hands each to a [`ClaimedProofJobHandler`], and submits the finished proofs back.
//! The worker is backend-agnostic: the SP1 and TEE workers are the same [`ProofWorker`]
//! instantiated with different handlers, and a defender process runs one instance per lane.
//!
//! ```text
//! PROVER-SERVICE --getNextProof(lane)--> PROOF-WORKER --handle_claimed_job--> BACKEND
//!        ^                                                                       |
//!        +-------------------- submitProof <-------------------------- ProofData +
//! ```
//!
//! The handler owns the entire proving workflow for a claimed job and may checkpoint
//! external proving progress through [`ProofJob::sessions`] so a worker restart can resume
//! rather than re-run completed phases.

mod backend;
mod worker;

pub use backend::{ClaimedProofJobHandler, JobSessions, ProofJob};
pub use worker::{
    DEFAULT_MAX_CONCURRENT_JOBS, DEFAULT_POLL_INTERVAL, ProofWorker, ProofWorkerConfig,
};
