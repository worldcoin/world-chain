//! The `prover-service` is the central orchestration layer between
//! Defenders and proof generation backends (SP1 and Nitro/TEE).
//!
//! # Architecture
//!
//! ```text
//!                         ┌──────────┐
//!                         │ DEFENDER │
//!                         └────┬─────┘
//!                              │
//!                              │ requestProof(type: SP1 | TEE)
//!                              ▼
//!              ┌─────────────────────────────────┐
//!              │         PROVER-SERVICE          │
//!              │  Queue + Routing + Proof Store  │
//!              └───────┬───────────────┬─────────┘
//!                      │               │
//!      getNextProof()  │               │  getNextProof()
//!                      ▼               ▼
//!             ┌────────────┐    ┌──────────────┐
//!             │ SP1-WORKER │    │ NITRO-WORKER │
//!             └─────┬──────┘    └──────┬───────┘
//!                   │                  │
//!                   │ Create witness   │
//!                   │ & submit job     │
//!                   ▼                  ▼
//!             ┌────────────┐    ┌───────────────┐
//!             │ SP1-PROVER │    │ NITRO-ENCLAVE │
//!             └─────┬──────┘    └──────┬────────┘
//!                   │                  │
//!                   │ Return proof     │
//!                   ▼                  ▼
//!             ┌────────────┐    ┌──────────────┐
//!             │ SP1-WORKER │    │ NITRO-WORKER │
//!             └─────┬──────┘    └──────┬───────┘
//!                   │                  │
//!                   └──── submitProof ─┘
//!                             │
//!                             ▼
//!              ┌─────────────────────────────────┐
//!              │         PROVER-SERVICE          │
//!              └─────────────────────────────────┘
//!                             │
//!                             │ getProof()
//!                             ▼
//!                        ┌──────────┐
//!                        │ DEFENDER │
//!                        └────┬─────┘
//!                             │
//!                             │ Craft transaction
//!                             │ containing proof
//!                             ▼
//!                       ┌────────────┐
//!                       │  ON-CHAIN  │
//!                       └────────────┘
//! ```
//!
//! # Request Lifecycle
//!
//! 1. A Defender requests a proof from the prover-service.
//! 2. The request is queued and routed according to the requested backend.
//! 3. Workers (`sp1-worker` or `nitro-worker`) poll for work using
//!    `getNextProof()`.
//! 4. The worker generates the witness and submits a proving job to the
//!    corresponding proving backend.
//! 5. The generated proof is returned to the worker.
//! 6. The worker submits the proof back via `submitProof()`.
//! 7. The Defender retrieves the completed proof using `getProof()`.
//! 8. The Defender crafts and submits the corresponding on-chain transaction.

mod config;
mod error;
mod rpc;
mod service;
mod traits;
mod types;

// re-exports
pub use config::{
    DEFAULT_BACKEND_POLL_INTERVAL, DEFAULT_LEASE_TIMEOUT, DEFAULT_MAX_ATTEMPTS,
    DEFAULT_MAX_FINISHED_JOBS, DEFAULT_MAX_QUEUE_LEN, ProverServiceConfig,
};
pub use error::{InvalidConfigError, ProofJobQueueError, ProofRequestError};
pub use rpc::{
    ProverServiceApiClient, ProverServiceApiServer, ProverServiceRpc, RpcProverServiceClient,
    start_rpc_server,
};
pub use service::{ProverService, ProverServiceInitError};
pub use traits::{ProofJobQueue, ProofRequester};
pub use types::{
    BackendProofId, BackendProofJobStatus, BackendProofState, BackendProofWork, BackendUpdate,
    LeaseToken, LeasedBackendProofWork, LeasedProofRequest, ProofBackend, ProofData, ProofRequest,
    ProofRequestId, ProofResponse, ProofStatus, ProofSubmissionLease,
};

#[cfg(test)]
mod tests;
