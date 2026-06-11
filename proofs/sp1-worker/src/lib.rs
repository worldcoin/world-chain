//! The `sp1-worker` is the SP1 proving worker of the World Chain defender stack.
//!
//! It is the SP1 analogue of the `nitro-worker`: it leases SP1 proof jobs from the
//! `prover-service`, builds the range witnesses over RPC, drives an SP1 prover (local CPU,
//! mock, or the Succinct proving network) through range and aggregation proving, and submits
//! the finished proof back to the `prover-service`. It performs no on-chain interaction —
//! the defender turns completed proofs into transactions.
//!
//! ```text
//! PROVER-SERVICE --getNextProof(Sp1)--> SP1-WORKER --witness + prove--> SP1-PROVER
//!        ^                                                                  |
//!        +---------------------------- submitProof <-----------------------+
//! ```

pub mod backend;

pub use backend::{Sp1Backend, Sp1BackendConfig};

// Re-exported so binaries and tests can build a worker without depending on
// `world-chain-proof-worker` directly.
pub use world_chain_proof_worker::{ProofJobBackend, ProofWorker, ProofWorkerConfig};
