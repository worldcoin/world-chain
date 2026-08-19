//! Range proofs are produced in `Compressed` mode so the aggregation guest can recursively
//! verify them; the aggregation proof mode is configurable (Groth16 for on-chain verification).

use std::time::Duration;

use crate::{SuccinctProverError, WorldSuccinctProver};
use alloy_primitives::{B256, U256};
use anyhow::{Context, bail};
use async_trait::async_trait;
pub use sp1_sdk::SP1ProofMode;
use sp1_sdk::{
    HashableKey, NetworkProver, ProveRequest, Prover, ProvingKey, SP1Proof,
    SP1ProofWithPublicValues, SP1ProvingKey, SP1Stdin,
    network::{
        Error as NetworkError, NetworkClient, NetworkMode, get_default_rpc_url_for_mode,
        proto::{GetProofRequestStatusResponse, types::FulfillmentStatus},
        signer::NetworkSigner,
    },
};
use world_chain_proof_core::types::AggregationInputs;
use world_chain_proof_sp1_types::{
    AggregationProofRequest, RangeProofRequest, Sp1ProofRequest, Sp1SessionStatus,
};

/// [`WorldSuccinctProver`] network implementation over the sp1-sdk network prover.
pub struct NetworkSuccinctProver {
    client: NetworkProver,
    range_pk: SP1ProvingKey,
    agg_pk: SP1ProvingKey,
    multi_block_vkey: [u32; 8],
    agg_mode: SP1ProofMode,
    limits: Option<NetworkProverLimits>,
    max_price_per_pgu: Option<u64>,
    auction_timeout: Option<Duration>,
    proof_timeout: Option<Duration>,
}

/// Upper bounds supplied to SP1 Network instead of estimating them by executing guests locally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofLimits {
    /// Maximum guest cycles accepted by the network request.
    pub cycle_limit: u64,
    /// Maximum prover gas units accepted by the network request.
    pub gas_limit: u64,
}

/// Separate limits for the much larger range guest and the small aggregation guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkProverLimits {
    /// Limits for the range guest.
    pub range: ProofLimits,
    /// Limits for the aggregation guest.
    pub aggregation: ProofLimits,
}

/// Optional request settings forwarded to the SP1 Network SDK.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkProofRequestConfig {
    /// Guest execution limits. When absent, the SDK estimates them locally.
    pub limits: Option<NetworkProverLimits>,
    /// Maximum auction price in PROVE base units per PGU.
    pub max_price_per_pgu: Option<u64>,
    /// Maximum time a request may remain unassigned. The SDK default applies when absent.
    pub auction_timeout: Option<Duration>,
    /// Overall proof-generation deadline. The SDK-derived default applies when absent.
    pub proof_timeout: Option<Duration>,
}

/// Lightweight client for reading an account's SP1 Network credit balance.
///
/// Unlike [`NetworkSuccinctProver`], this does not initialize the SP1 light node or set up the
/// embedded proving programs. It uses the same credentials and `NETWORK_RPC_URL` selection as
/// the SDK's network prover builder.
#[derive(Clone)]
pub struct NetworkCreditClient {
    client: NetworkClient,
}

/// Initialized SP1 network signer and endpoint configuration reusable across network clients.
#[derive(Clone)]
pub struct NetworkConnection {
    signer: NetworkSigner,
    rpc_url: String,
    mode: NetworkMode,
}

/// Signer type used by the SP1 network clients.
#[derive(Clone, Copy, Debug)]
pub enum SignerType {
    Local,
    AwsKms,
}

impl NetworkConnection {
    /// Initializes a reusable SP1 network connection.
    pub async fn new(secret: &str, signer_type: SignerType) -> anyhow::Result<Self> {
        let signer = match signer_type {
            SignerType::Local => NetworkSigner::local(secret).context("invalid SP1 private key")?,
            SignerType::AwsKms => {
                // Keep the AWS SDK future boxed. Storing it inline makes the enclosing
                // devnet futures exceed rustc's query-depth limit during workspace clippy.
                Box::pin(NetworkSigner::aws_kms(secret))
                    .await
                    .context("failed to initialize AWS KMS signer for SP1 SDK")?
            }
        };
        // PROVE deposits fund the auction-based SP1 Network --> Network = Mainnet
        let mode = NetworkMode::Mainnet;
        let rpc_url =
            std::env::var("NETWORK_RPC_URL").unwrap_or_else(|_| get_default_rpc_url_for_mode(mode));

        Ok(Self {
            signer,
            rpc_url,
            mode,
        })
    }
}

impl NetworkCreditClient {
    /// Creates a mainnet credit client for the provided signer.
    pub async fn new(secret: &str, signer_type: SignerType) -> anyhow::Result<Self> {
        let connection = NetworkConnection::new(secret, signer_type).await?;

        Ok(Self::from_connection(connection))
    }

    /// Creates a credit client from an initialized network connection.
    pub fn from_connection(connection: NetworkConnection) -> Self {
        Self {
            client: NetworkClient::new(connection.signer, connection.rpc_url, connection.mode),
        }
    }

    /// Returns the account's available SP1 Network credits in PROVE base units.
    pub async fn get_balance(&self) -> anyhow::Result<U256> {
        self.client
            .get_balance()
            .await
            .context("failed to get SP1 Network credit balance")
    }
}

impl NetworkSuccinctProver {
    /// Creates the prover using caller-supplied ELFs. Use this in production binaries with
    /// ELFs embedded at compile time via `world_chain_proof_sp1_elfs`.
    pub async fn new(
        agg_mode: SP1ProofMode,
        secret: &str,
        signer_type: SignerType,
    ) -> anyhow::Result<Self> {
        let connection = NetworkConnection::new(secret, signer_type).await?;

        Self::from_connection(agg_mode, connection).await
    }

    /// Creates the prover from an initialized network connection.
    pub async fn from_connection(
        agg_mode: SP1ProofMode,
        connection: NetworkConnection,
    ) -> anyhow::Result<Self> {
        Self::from_connection_with_network_config(
            agg_mode,
            connection,
            NetworkProofRequestConfig::default(),
        )
        .await
    }

    /// Creates the prover with optional SP1 Network request settings.
    pub async fn from_connection_with_network_config(
        agg_mode: SP1ProofMode,
        connection: NetworkConnection,
        request_config: NetworkProofRequestConfig,
    ) -> anyhow::Result<Self> {
        let NetworkProofRequestConfig {
            limits,
            max_price_per_pgu,
            auction_timeout,
            proof_timeout,
        } = request_config;
        let range_elf = world_chain_proof_sp1_elfs::range_elf();
        let agg_elf = world_chain_proof_sp1_elfs::aggregation_elf();
        let client =
            NetworkProver::new(connection.signer, &connection.rpc_url, connection.mode).await;
        let range_pk = client
            .setup(range_elf.clone())
            .await
            .context("range program setup failed")?;
        let agg_pk = client
            .setup(agg_elf.clone())
            .await
            .context("aggregation program setup failed")?;
        let multi_block_vkey = range_pk.verifying_key().hash_u32();

        Ok(Self {
            client,
            range_pk,
            agg_pk,
            multi_block_vkey,
            agg_mode,
            limits,
            max_price_per_pgu,
            auction_timeout,
            proof_timeout,
        })
    }

    async fn request_range_proof(&self, request: RangeProofRequest) -> anyhow::Result<String> {
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(request.witness_rkyv);

        let mut proof_request = self.client.prove(&self.range_pk, stdin).compressed();
        if let Some(limits) = self.limits {
            proof_request = proof_request
                .cycle_limit(limits.range.cycle_limit)
                .gas_limit(limits.range.gas_limit)
                .skip_simulation(true);
        }
        if let Some(max_price_per_pgu) = self.max_price_per_pgu {
            proof_request = proof_request.max_price_per_pgu(max_price_per_pgu);
        }
        if let Some(proof_timeout) = self.proof_timeout {
            proof_request = proof_request.timeout(proof_timeout);
        }

        let backend_session_id = proof_request
            .request()
            .await
            .context("request range proving failed")?;

        Ok(backend_session_id.to_string())
    }

    async fn request_aggregation_proof(
        &self,
        request: AggregationProofRequest,
    ) -> anyhow::Result<String> {
        let mut stdin = SP1Stdin::new();
        let range_vk = self.range_pk.verifying_key().vk.clone();
        for proof_bytes in &request.range_proofs {
            let proof: sp1_sdk::SP1ProofWithPublicValues =
                bincode::deserialize(proof_bytes).context("range proof deserialization failed")?;
            let SP1Proof::Compressed(inner) = proof.proof else {
                return Err(SuccinctProverError::NotCompressed.into());
            };
            stdin.write_proof(*inner, range_vk.clone());
        }
        stdin.write(&request.inputs);
        stdin.write_vec(request.l1_headers_cbor);

        let mut proof_request = self.client.prove(&self.agg_pk, stdin).mode(self.agg_mode);
        if let Some(limits) = self.limits {
            proof_request = proof_request
                .cycle_limit(limits.aggregation.cycle_limit)
                .gas_limit(limits.aggregation.gas_limit)
                .skip_simulation(true);
        }
        if let Some(max_price_per_pgu) = self.max_price_per_pgu {
            proof_request = proof_request.max_price_per_pgu(max_price_per_pgu);
        }
        if let Some(proof_timeout) = self.proof_timeout {
            proof_request = proof_request.timeout(proof_timeout);
        }

        let backend_session_id = proof_request
            .request()
            .await
            .context("aggregation proving failed")?;

        Ok(backend_session_id.to_string())
    }

    /// Fetch the network session state and any proof returned by the SP1 Network.
    pub async fn get_network_proof_status(
        &self,
        backend_session_id: &str,
    ) -> anyhow::Result<(Sp1SessionStatus, Option<SP1ProofWithPublicValues>)> {
        let proof_id = parse_proof_id(backend_session_id)?;
        let (status, proof) = self
            .client
            .get_proof_status(proof_id)
            .await
            .context("failed to get network proof status")?;
        let sp1_status = sp1_status(&status);
        Ok((sp1_status, proof))
    }
}

#[async_trait]
impl WorldSuccinctProver for NetworkSuccinctProver {
    fn supports_persistent_sessions(&self) -> bool {
        true
    }

    async fn submit(&self, request: Sp1ProofRequest) -> anyhow::Result<String> {
        match request {
            Sp1ProofRequest::Range(range_request) => self.request_range_proof(range_request).await,
            Sp1ProofRequest::Aggregation(session_request) => {
                let agg_request = AggregationProofRequest {
                    inputs: AggregationInputs {
                        transition_public_values: session_request.transition_public_values,
                        latest_l1_checkpoint_head: session_request.latest_l1_checkpoint_head,
                        multi_block_vkey: self.multi_block_vkey,
                    },
                    l1_headers_cbor: session_request.l1_headers_cbor,
                    range_proofs: session_request.range_proofs,
                };
                self.request_aggregation_proof(agg_request).await
            }
        }
    }

    async fn poll(&self, session_id: &str) -> anyhow::Result<Sp1SessionStatus> {
        let (sp1_status, _maybe_proof) = self.get_network_proof_status(session_id).await?;
        Ok(sp1_status)
    }

    async fn download(&self, session_id: &str) -> anyhow::Result<SP1ProofWithPublicValues> {
        let (sp1_status, maybe_proof) = self.get_network_proof_status(session_id).await?;
        match sp1_status {
            Sp1SessionStatus::Completed => maybe_proof.ok_or_else(|| {
                anyhow::anyhow!("network proof {session_id} is fulfilled but no proof was returned")
            }),
            Sp1SessionStatus::Running => {
                bail!("network proof {session_id} is not fulfilled yet");
            }
            Sp1SessionStatus::Failed(reason) => bail!("{reason}"),
            Sp1SessionStatus::NotFound => {
                bail!("network proof {session_id} was not found");
            }
        }
    }

    async fn wait(&self, session_id: &str) -> anyhow::Result<SP1ProofWithPublicValues> {
        let proof_id = parse_proof_id(session_id)?;
        // SP1 6.1.0 can misclassify a fulfilled proof polled after its deadline as timed out.
        // This accepted limitation is fixed by upgrading to SP1 6.2.0 or newer (#2737).
        self.client
            // The request carries its immutable network deadline. Passing no additional local
            // timeout keeps restart recovery anchored to that original deadline.
            .wait_proof(proof_id, None, self.auction_timeout)
            .await
            .map_err(|error| map_network_wait_error(error, session_id))
    }
}

fn map_network_wait_error(error: anyhow::Error, session_id: &str) -> anyhow::Error {
    let Some(network_error) = error.downcast_ref::<NetworkError>() else {
        return error.context(format!("waiting for SP1 Network request {session_id}"));
    };

    match network_error {
        NetworkError::RequestAuctionTimedOut { .. } => {
            SuccinctProverError::RequestAuctionTimedOut {
                session_id: session_id.to_string(),
            }
            .into()
        }
        NetworkError::RequestTimedOut { .. } => SuccinctProverError::RequestTimedOut {
            session_id: session_id.to_string(),
        }
        .into(),
        NetworkError::RequestUnexecutable { .. } => SuccinctProverError::RequestUnexecutable {
            session_id: session_id.to_string(),
        }
        .into(),
        NetworkError::RequestUnfulfillable { .. } => SuccinctProverError::RequestUnfulfillable {
            session_id: session_id.to_string(),
        }
        .into(),
        _ => error.context(format!("waiting for SP1 Network request {session_id}")),
    }
}

/// Parse a network proof ID from its hex string representation.
fn parse_proof_id(proof_id: &str) -> anyhow::Result<B256> {
    proof_id
        .parse::<B256>()
        .map_err(|e| anyhow::anyhow!("invalid network proof ID: {e}"))
}

/// Map an SP1 Network proof status response to the sp1 session status.
fn sp1_status(status: &GetProofRequestStatusResponse) -> Sp1SessionStatus {
    match FulfillmentStatus::try_from(status.fulfillment_status()) {
        Ok(FulfillmentStatus::Fulfilled) => Sp1SessionStatus::Completed,
        Ok(FulfillmentStatus::Unfulfillable) => Sp1SessionStatus::Failed(format!(
            "proof unfulfillable, execution_status={}",
            status.execution_status()
        )),
        Ok(FulfillmentStatus::Assigned)
        | Ok(FulfillmentStatus::Requested)
        | Ok(FulfillmentStatus::UnspecifiedFulfillmentStatus) => Sp1SessionStatus::Running,
        Err(_) => Sp1SessionStatus::Failed(format!(
            "unknown network proof fulfillment status: {}",
            status.fulfillment_status()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sdk_auction_timeout_to_resubmittable_error() {
        let error = anyhow::Error::new(NetworkError::RequestAuctionTimedOut {
            request_id: vec![1; 32],
        });

        let mapped = map_network_wait_error(error, "0x01");
        let mapped = mapped
            .downcast_ref::<SuccinctProverError>()
            .expect("SDK timeout should map to a structured prover error");

        assert!(matches!(
            mapped,
            SuccinctProverError::RequestAuctionTimedOut { session_id } if session_id == "0x01"
        ));
        assert!(mapped.should_resubmit());
    }

    #[test]
    fn preserves_non_terminal_sdk_errors() {
        let error = anyhow::Error::new(NetworkError::SimulationFailed);

        let mapped = map_network_wait_error(error, "0x02");

        assert!(mapped.downcast_ref::<NetworkError>().is_some());
        assert!(mapped.downcast_ref::<SuccinctProverError>().is_none());
    }
}
