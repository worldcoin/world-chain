use alloy_primitives::{Address, B256, BlockNumber, Bytes, keccak256};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The proving backend that should generate a proof.
///
/// This is a routing concept internal to the proving pipeline: it selects
/// which worker pool picks up the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProofBackend {
    /// SP1 zkVM validity proof.
    Sp1,
    /// AWS Nitro TEE attestation.
    Nitro,
}

impl std::fmt::Display for ProofBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sp1 => write!(f, "sp1"),
            Self::Nitro => write!(f, "nitro"),
        }
    }
}

impl ProofBackend {
    /// Stable database representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sp1 => "sp1",
            Self::Nitro => "nitro",
        }
    }
}

impl TryFrom<&str> for ProofBackend {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "sp1" => Ok(Self::Sp1),
            "nitro" => Ok(Self::Nitro),
            other => Err(format!("unknown proof backend {other:?}")),
        }
    }
}

/// The proof request sent from a defender
/// to the `prover-service`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofRequest {
    /// The backend that should generate the proof.
    pub backend: ProofBackend,
    /// The `WorldChainProofSystemGame` contract address the proof defends.
    pub game: Address,
    /// The output root claim to prove.
    pub root_claim: B256,
    /// The L2 block number the `root_claim` refers to.
    pub l2_block_number: BlockNumber,
    /// The L1 head hash pinning the witness data.
    pub l1_head: B256,
}

impl ProofRequest {
    /// Compute the deterministic identifier of this request.
    ///
    /// The id is a commitment to every request field, so requesting the
    /// same proof twice yields the same id and the duplicate is deduplicated
    /// by the `prover-service`.
    #[must_use]
    pub fn id(&self) -> ProofRequestId {
        let mut buf = Vec::with_capacity(1 + 20 + 32 + 8 + 32);
        buf.push(self.backend as u8);
        buf.extend_from_slice(self.game.as_slice());
        buf.extend_from_slice(self.root_claim.as_slice());
        buf.extend_from_slice(&self.l2_block_number.to_be_bytes());
        buf.extend_from_slice(self.l1_head.as_slice());
        ProofRequestId(keccak256(buf))
    }
}

/// The unique identifier of a proof request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProofRequestId(pub B256);

impl std::fmt::Display for ProofRequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Status of a proof request
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofStatus {
    /// Proof request has been created but not yet queued.
    Created,
    /// Proof is actively being generated.
    Running,
    /// Proof generation completed successfully.
    Succeeded,
    /// Proof generation failed.
    Failed,
}

impl ProofStatus {
    /// Convert enum to static string representation
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Running => "RUNNING",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
        }
    }
}

impl std::fmt::Display for ProofStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl TryFrom<&str> for ProofStatus {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "CREATED" => Ok(Self::Created),
            "RUNNING" => Ok(Self::Running),
            "SUCCEEDED" => Ok(Self::Succeeded),
            "FAILED" => Ok(Self::Failed),
            other => Err(format!("Unknown proof status: {other}")),
        }
    }
}

/// Worker-owned job lifecycle status, distinct from requester [`ProofStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofJobStatus {
    /// Job is claimable and not currently owned by any worker.
    Pending,
    /// Job is currently claimed by a worker under an unexpired lock.
    Claimed,
    /// Job completed successfully through the worker API.
    Succeeded,
    /// Job failed terminally.
    Failed,
}

impl ProofJobStatus {
    /// Convert enum to static string representation.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Claimed => "CLAIMED",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
        }
    }
}

impl std::fmt::Display for ProofJobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl TryFrom<&str> for ProofJobStatus {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "PENDING" => Ok(Self::Pending),
            "CLAIMED" => Ok(Self::Claimed),
            "SUCCEEDED" => Ok(Self::Succeeded),
            "FAILED" => Ok(Self::Failed),
            other => Err(format!("Unknown proof job status: {other}")),
        }
    }
}

/// The proof payload generated by a backend.
///
/// The `prover-service` stores and forwards these payloads without
/// interpreting them, verification happens on-chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofData {
    /// SP1 zkVM validity proof.
    Sp1 {
        /// Serialized SP1 proof.
        proof: Bytes,
        /// Public values committed by the program.
        public_values: Bytes,
    },
    /// AWS Nitro TEE attestation.
    Nitro {
        /// Attestation document produced by the enclave.
        attestation: Bytes,
        /// Enclave signature over the proven outputs.
        signature: Bytes,
    },
}

impl ProofData {
    /// The backend that produced this proof.
    #[must_use]
    pub const fn backend(&self) -> ProofBackend {
        match self {
            Self::Sp1 { .. } => ProofBackend::Sp1,
            Self::Nitro { .. } => ProofBackend::Nitro,
        }
    }
}

/// The response sent by the `prover-service` to
/// a defender that requests the proof back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofResponse {
    Succeeded(SucceededProofResponse),
    Failed(FailedProofResponse),
    Pending(PendingProofResponse),
}

/// The succeeded proof response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SucceededProofResponse {
    /// The identifier of the proof request this proof answers.
    pub id: ProofRequestId,
    /// The actual proof.
    pub proof: ProofData,
}

/// The failed proof response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedProofResponse {
    /// The identifier of the proof request.
    id: ProofRequestId,
    /// Failure reason.
    reason: String,
}

/// The pending proof response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProofResponse {
    /// The identifier of the proof request.
    id: ProofRequestId,
    /// Current proof status.
    status: ProofStatus,
}

/// External backend request identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BackendProofId(pub B256);

impl std::fmt::Display for BackendProofId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque token proving ownership of a locked row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LockId(pub Uuid);

impl LockId {
    /// Generate a fresh lock id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for LockId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for LockId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Initial proof request locked from `proof_requests`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedProofRequest {
    /// The user-facing proof request.
    pub request: ProofRequest,
    /// Token required for updates to the locked proof job.
    pub lock_id: LockId,
}

/// The session type used to differentiate the proof type
/// returned by SP1 backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionType {
    /// The range proof.
    Stark,
    /// The final snark groth16 proof.
    Snark,
}

impl SessionType {
    /// Convert enum to static string representation
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Stark => "STARK",
            Self::Snark => "SNARK",
        }
    }
}

impl TryFrom<&str> for SessionType {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "STARK" => Ok(Self::Stark),
            "SNARK" => Ok(Self::Snark),
            other => Err(format!("Unknown session type: {other}")),
        }
    }
}

/// A backend session tracked in the prover service for a proof job.
///
/// Workers record the backend-issued identifier (for example an SP1 cluster or
/// network proof id) so a restart or reclaim resumes the in-flight backend job
/// rather than re-running it. The worker itself holds no local state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendSession {
    /// Backend-specific session identifier used to resume polling.
    pub backend_session_id: String,
    /// Current backend session lifecycle status.
    pub status: BackendSessionStatus,
}

/// Lifecycle status of a tracked backend session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendSessionStatus {
    /// Reservation placeholder before the backend job has been submitted.
    Submitting,
    /// Backend session is actively running.
    Running,
    /// Backend session completed successfully.
    Completed,
    /// Backend session failed.
    Failed,
}

impl BackendSessionStatus {
    /// Convert enum to static string representation
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Submitting => "SUBMITTING",
            Self::Running => "RUNNING",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
        }
    }

    /// Whether this status represents a terminal backend session.
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

impl TryFrom<&str> for BackendSessionStatus {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "SUBMITTING" => Ok(Self::Submitting),
            "RUNNING" => Ok(Self::Running),
            "COMPLETED" => Ok(Self::Completed),
            "FAILED" => Ok(Self::Failed),
            other => Err(format!("Unknown session status: {other}")),
        }
    }
}
