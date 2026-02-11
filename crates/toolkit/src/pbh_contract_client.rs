use alloy_rpc_client::BatchRequest;

use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_sol_types::{sol_data, SolCall, SolType};
use chrono::NaiveDate;
use eyre::eyre::{bail, Context};
use semaphore_rs::{identity::Identity, Field};
use serde_json::json;
use tracing::{debug, info, warn};
use world_chain_pbh::{
    contracts::IPBHEntryPoint,
    date_marker::DateMarker,
    external_nullifier::{EncodedExternalNullifier, ExternalNullifier},
};

const JSON_RPC_BATCH_SIZE: usize = 256;
const MAX_PBH_NONCE_LIMIT: u16 = 512;

/// A client to query PBH contract state via Alloy provider RPC calls.
#[derive(Debug, Clone)]
pub struct PbhContractClient {
    rpc_endpoint: String,
}

impl PbhContractClient {
    /// Creates a new client.
    pub fn new(rpc_endpoint: impl Into<String>) -> Self {
        Self {
            rpc_endpoint: rpc_endpoint.into(),
        }
    }

    /// Finds unspent PBH nonces for a given identity and date by querying spent nullifiers
    /// from the PBH entrypoint contract.
    pub async fn find_available_pbh_nonces(
        &self,
        pbh_entrypoint: Address,
        identity: &Identity,
        date: NaiveDate,
    ) -> eyre::Result<Vec<u16>> {
        info!(
            rpc_endpoint = %self.rpc_endpoint,
            pbh_entrypoint = %pbh_entrypoint,
            date = %date,
            "starting available PBH nonce query"
        );
        let provider = ProviderBuilder::new().connect(&self.rpc_endpoint).await?;

        // Query nonce limit first, then evaluate only this range.
        let reported_pbh_nonce_limit = self.pbh_nonce_limit(&provider, pbh_entrypoint).await?;
        let pbh_nonce_limit = reported_pbh_nonce_limit.min(MAX_PBH_NONCE_LIMIT);
        if reported_pbh_nonce_limit != pbh_nonce_limit {
            warn!(
                reported_pbh_nonce_limit,
                capped_pbh_nonce_limit = pbh_nonce_limit,
                max_allowed_nonce_limit = MAX_PBH_NONCE_LIMIT,
                "capping PBH nonce limit for request safety"
            );
        }
        info!(pbh_nonce_limit, "resolved PBH nonce limit");

        let date_marker = DateMarker::from(date);

        let mut nonce_and_nullifier_hashes = Vec::with_capacity(pbh_nonce_limit as usize);
        for nonce in 0..pbh_nonce_limit {
            let external_nullifier = ExternalNullifier::with_date_marker(date_marker, nonce);
            let external_nullifier_hash = EncodedExternalNullifier::from(external_nullifier).0;
            let nullifier_hash =
                semaphore_rs::protocol::generate_nullifier_hash(identity, external_nullifier_hash);
            nonce_and_nullifier_hashes.push((nonce, nullifier_hash));
        }

        let mut available_nonces = Vec::with_capacity(nonce_and_nullifier_hashes.len());
        info!(
            nullifier_count = nonce_and_nullifier_hashes.len(),
            batch_size = JSON_RPC_BATCH_SIZE,
            "prepared nullifier hashes"
        );

        for (chunk_idx, chunk) in nonce_and_nullifier_hashes
            .chunks(JSON_RPC_BATCH_SIZE)
            .enumerate()
        {
            let nullifier_hashes: Vec<Field> = chunk
                .iter()
                .map(|(_, nullifier_hash)| *nullifier_hash)
                .collect();
            debug!(
                chunk_index = chunk_idx,
                chunk_size = nullifier_hashes.len(),
                "querying nullifier hash batch"
            );
            let spent_at_blocks = self
                .nullifier_spent_at_blocks_batch(&provider, pbh_entrypoint, &nullifier_hashes)
                .await?;

            let available_before = available_nonces.len();
            for ((nonce, _), spent_at_block) in chunk.iter().zip(spent_at_blocks.into_iter()) {
                if spent_at_block.is_zero() {
                    available_nonces.push(*nonce);
                }
            }
            debug!(
                chunk_index = chunk_idx,
                available_in_chunk = available_nonces.len() - available_before,
                "processed nullifier hash batch"
            );
        }

        info!(
            available_nonce_count = available_nonces.len(),
            "completed PBH nonce query"
        );
        Ok(available_nonces)
    }

    async fn pbh_nonce_limit<P: Provider>(
        &self,
        provider: &P,
        pbh_entrypoint: Address,
    ) -> eyre::Result<u16> {
        let calldata = IPBHEntryPoint::numPbhPerMonthCall {}.abi_encode();
        let output = self.eth_call(provider, pbh_entrypoint, calldata).await?;
        sol_data::Uint::<16>::abi_decode(&output).context("failed to decode numPbhPerMonth")
    }

    async fn nullifier_spent_at_blocks_batch(
        &self,
        provider: &impl Provider,
        pbh_entrypoint: Address,
        nullifier_hashes: &[Field],
    ) -> eyre::Result<Vec<U256>> {
        let mut batch = BatchRequest::new(provider.client());
        let mut waiters = Vec::with_capacity(nullifier_hashes.len());

        for nullifier_hash in nullifier_hashes {
            let calldata = IPBHEntryPoint::nullifierHashesCall {
                nullifierHash: *nullifier_hash,
            }
            .abi_encode();

            let tx = json!({
                "to": format!("{pbh_entrypoint:#x}"),
                "data": format!("0x{}", hex::encode(calldata)),
            });

            let waiter = batch
                .add_call::<_, String>("eth_call", &(tx, "latest"))
                .context("failed to queue batch eth_call")?;

            waiters.push(waiter);
        }

        if let Err(err) = batch.send().await {
            return Err(err).context("failed to send batch eth_call request");
        }

        let mut spent_at_blocks = Vec::with_capacity(waiters.len());
        for (_waiter_idx, waiter) in waiters.into_iter().enumerate() {
            let result = match waiter.await {
                Ok(result) => result,
                Err(err) => {
                    return Err(err).context("failed to receive batch eth_call response");
                }
            };
            let output = decode_hex_result(&result)?;
            let spent_at_block = sol_data::Uint::<256>::abi_decode(&output)
                .context("failed to decode nullifierHashes batch response")?;
            spent_at_blocks.push(spent_at_block);
        }

        Ok(spent_at_blocks)
    }

    async fn eth_call<P: Provider>(
        &self,
        provider: &P,
        to: Address,
        data: Vec<u8>,
    ) -> eyre::Result<Vec<u8>> {
        let tx = json!({
            "to": format!("{to:#x}"),
            "data": format!("0x{}", hex::encode(data)),
        });

        let result: String = provider
            .client()
            .request("eth_call", (tx, "latest"))
            .await
            .context("failed to call eth_call")?;

        let bytes = decode_hex_result(&result)?;
        if bytes.is_empty() {
            bail!("empty eth_call response for contract read");
        }

        Ok(bytes)
    }
}

fn decode_hex_result(result: &str) -> eyre::Result<Vec<u8>> {
    if result == "0x" {
        return Ok(Vec::new());
    }

    hex::decode(result.trim_start_matches("0x")).context("failed to decode hex eth_call result")
}
