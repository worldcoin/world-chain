use std::error::Error;

use crate::{core::WorldChainEthApiExt, sequencer::SequencerClient};
use alloy_consensus::BlockHeader;
use alloy_eips::{eip2718::Eip2718Result, BlockId, Decodable2718, Encodable2718, Typed2718};
use alloy_primitives::{map::HashMap, StorageKey, TxHash};
use alloy_rlp::{Decodable, Encodable};
use alloy_rpc_types::erc4337::{AccountStorage, TransactionConditional};
use jsonrpsee::{
    core::{async_trait, RpcResult},
    types::{ErrorCode, ErrorObject, ErrorObjectOwned},
};
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    api::Block,
    core::primitives::{transaction::signed::RecoveryError, InMemorySize, SignedTransaction},
    rpc::{
        api::eth::{AsEthApiError, FromEthApiError},
        server_types::eth::{utils::recover_raw_transaction, EthApiError},
    },
    transaction_pool::{PoolTransaction, TransactionOrigin, TransactionPool},
};
use reth_optimism_node::txpool::OpPooledTransaction;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use revm_primitives::{
    map::FbBuildHasher, AccessList, Address, Bytes, FixedBytes, PrimitiveSignature,
    SignedAuthorization, TxKind, B256, U256,
};
use serde::{Deserialize, Serialize};
use world_chain_builder_pbh::PBHSidecar;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;

pub const PBH_TX_TYPE: u8 = 0xE0;

#[derive(Debug, Deserialize, Serialize)]
pub struct WorldChainTxEnvelope {
    inner: OpTxEnvelope,
    pbh_sidecar: Option<PBHSidecar>,
}

impl Encodable for WorldChainTxEnvelope {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.network_encode(out)
    }

    fn length(&self) -> usize {
        self.network_len()
    }
}

impl Decodable for WorldChainTxEnvelope {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(Self::network_decode(buf)?)
    }
}

impl Encodable2718 for WorldChainTxEnvelope {
    fn type_flag(&self) -> Option<u8> {
        if self.pbh_sidecar.is_some() {
            Some(PBH_TX_TYPE)
        } else {
            self.inner.type_flag()
        }
    }

    fn encode_2718_len(&self) -> usize {
        if self.pbh_sidecar.is_some() {
            todo!("TODO:")
        } else {
            self.inner.encode_2718_len()
        }
    }

    fn encode_2718(&self, out: &mut dyn alloy_rlp::BufMut) {
        if self.pbh_sidecar.is_some() {
            todo!("TODO:")
        } else {
            self.inner.encode_2718(out)
        }
    }

    fn trie_hash(&self) -> B256 {
        self.inner.trie_hash()
    }
}

impl Decodable2718 for WorldChainTxEnvelope {
    fn typed_decode(ty: u8, buf: &mut &[u8]) -> Eip2718Result<Self> {
        if ty == PBH_TX_TYPE {
            todo!("TODO:")
        } else {
            let inner = OpTxEnvelope::typed_decode(ty, buf)?;

            Ok(Self {
                inner,
                pbh_sidecar: None,
            })
        }
    }

    fn fallback_decode(buf: &mut &[u8]) -> Eip2718Result<Self> {
        let inner = OpTxEnvelope::fallback_decode(buf)?;

        Ok(Self {
            inner,
            pbh_sidecar: None,
        })
    }
}

impl Typed2718 for WorldChainTxEnvelope {
    fn ty(&self) -> u8 {
        if self.pbh_sidecar.is_some() {
            PBH_TX_TYPE
        } else {
            self.inner.ty()
        }
    }
}

impl alloy_consensus::Transaction for WorldChainTxEnvelope {
    fn chain_id(&self) -> Option<u64> {
        self.inner.chain_id()
    }

    fn nonce(&self) -> u64 {
        self.inner.nonce()
    }

    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    fn gas_price(&self) -> Option<u128> {
        self.inner.gas_price()
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.inner.max_fee_per_gas()
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.inner.max_priority_fee_per_gas()
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.inner.max_fee_per_blob_gas()
    }

    fn priority_fee_or_price(&self) -> u128 {
        self.inner.priority_fee_or_price()
    }

    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        self.inner.effective_gas_price(base_fee)
    }

    fn is_dynamic_fee(&self) -> bool {
        self.inner.is_dynamic_fee()
    }

    fn kind(&self) -> TxKind {
        self.inner.kind()
    }

    fn is_create(&self) -> bool {
        self.inner.is_create()
    }

    fn to(&self) -> Option<Address> {
        self.inner.to()
    }

    fn value(&self) -> U256 {
        self.inner.value()
    }

    fn input(&self) -> &Bytes {
        self.inner.input()
    }

    fn access_list(&self) -> Option<&AccessList> {
        self.inner.access_list()
    }

    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        self.inner.blob_versioned_hashes()
    }

    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        self.inner.authorization_list()
    }
}

#[async_trait]
pub trait EthTransactionsExt {
    /// Extension of [`FromEthApiError`], with network specific errors.
    type Error: Into<jsonrpsee_types::error::ErrorObject<'static>>
        + FromEthApiError
        + AsEthApiError
        + Error
        + Send
        + Sync;

    async fn send_raw_transaction_conditional(
        &self,
        tx: Bytes,
        options: TransactionConditional,
    ) -> Result<B256, Self::Error>;

    async fn send_raw_transaction(&self, tx: Bytes) -> Result<B256, Self::Error>;
}

#[async_trait]
impl<Pool, Client> EthTransactionsExt for WorldChainEthApiExt<Pool, Client>
where
    Pool: TransactionPool<Transaction = WorldChainPooledTransaction> + Clone + 'static,
    Client: BlockReaderIdExt + StateProviderFactory + 'static,
{
    type Error = EthApiError;

    async fn send_raw_transaction_conditional(
        &self,
        tx: Bytes,
        options: TransactionConditional,
    ) -> Result<B256, Self::Error> {
        validate_conditional_options(&options, self.provider()).map_err(Self::Error::other)?;

        let recovered = recover_raw_transaction(&tx)?;
        let mut pool_transaction: WorldChainPooledTransaction =
            OpPooledTransaction::from_pooled(recovered).into();
        pool_transaction.inner = pool_transaction.inner.with_conditional(options.clone());

        // submit the transaction to the pool with a `Local` origin
        let hash = self
            .pool()
            .add_transaction(TransactionOrigin::Local, pool_transaction)
            .await
            .map_err(Self::Error::from_eth_err)?;

        if let Some(client) = self.raw_tx_forwarder().as_ref() {
            tracing::debug!( target: "rpc::eth",  "forwarding raw conditional transaction to");
            let _ = client.forward_raw_transaction_conditional(&tx, options).await.inspect_err(|err| {
                        tracing::debug!(target: "rpc::eth", %err, hash=?*hash, "failed to forward raw conditional transaction");
                    });
        }
        Ok(hash)
    }

    async fn send_raw_transaction(&self, tx: Bytes) -> Result<B256, Self::Error> {
        // TODO: recover into world chain pool transaction, note that these txs will not get peered unless network primitives are configured
        // let mut data: &[u8] = &tx;
        // let tx_envelope = WorldChainTxEnvelope::decode_2718(&mut data)
        //     .map_err(|_| EthApiError::FailedToDecodeSignedTransaction)?;

        // TODO: into worldchainpooledtransaction

        let recovered = recover_raw_transaction(&tx)?;
        let pool_transaction: WorldChainPooledTransaction =
            OpPooledTransaction::from_pooled(recovered).into();

        // submit the transaction to the pool with a `Local` origin
        let hash = self
            .pool()
            .add_transaction(TransactionOrigin::Local, pool_transaction)
            .await
            .map_err(Self::Error::from_eth_err)?;

        if let Some(client) = self.raw_tx_forwarder().as_ref() {
            tracing::debug!( target: "rpc::eth",  "forwarding raw transaction to");
            let _ = client.forward_raw_transaction(&tx).await.inspect_err(|err| {
                        tracing::debug!(target: "rpc::eth", %err, hash=?*hash, "failed to forward raw transaction");
                    });
        }
        Ok(hash)
    }
}

impl<Pool, Client> WorldChainEthApiExt<Pool, Client>
where
    Pool: TransactionPool<Transaction = WorldChainPooledTransaction> + Clone + 'static,
    Client: BlockReaderIdExt + StateProviderFactory + 'static,
{
    pub fn new(pool: Pool, client: Client, sequencer_client: Option<SequencerClient>) -> Self {
        Self {
            pool,
            client,
            sequencer_client,
        }
    }

    pub fn provider(&self) -> &Client {
        &self.client
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    pub fn raw_tx_forwarder(&self) -> Option<&SequencerClient> {
        self.sequencer_client.as_ref()
    }
}

/// Validates the conditional inclusion options provided by the client.
///
/// reference for the implementation <https://notes.ethereum.org/@yoav/SkaX2lS9j#>
/// See also <https://pkg.go.dev/github.com/aK0nshin/go-ethereum/arbitrum_types#ConditionalOptions>
pub fn validate_conditional_options<Client>(
    options: &TransactionConditional,
    provider: &Client,
) -> RpcResult<()>
where
    Client: BlockReaderIdExt + StateProviderFactory,
{
    let latest = provider
        .block_by_id(BlockId::latest())
        .map_err(|e| ErrorObject::owned(ErrorCode::InternalError.code(), e.to_string(), Some("")))?
        .ok_or(ErrorObjectOwned::from(ErrorCode::InternalError))?;

    validate_known_accounts(
        &options.known_accounts,
        latest.header().number().into(),
        provider,
    )?;

    if let Some(min_block) = options.block_number_min {
        if min_block > latest.header().number() {
            return Err(ErrorCode::from(-32003).into());
        }
    }

    if let Some(max_block) = options.block_number_max {
        if max_block < latest.header().number() {
            return Err(ErrorCode::from(-32003).into());
        }
    }

    if let Some(min_timestamp) = options.timestamp_min {
        if min_timestamp > latest.header().timestamp() {
            return Err(ErrorCode::from(-32003).into());
        }
    }

    if let Some(max_timestamp) = options.timestamp_max {
        if max_timestamp < latest.header().timestamp() {
            return Err(ErrorCode::from(-32003).into());
        }
    }

    Ok(())
}

/// Validates the account storage slots/storage root provided by the client
///
/// Matches the current state of the account storage slots/storage root.
pub fn validate_known_accounts<Client>(
    known_accounts: &HashMap<Address, AccountStorage, FbBuildHasher<20>>,
    latest: BlockId,
    provider: &Client,
) -> RpcResult<()>
where
    Client: BlockReaderIdExt + StateProviderFactory,
{
    let state = provider.state_by_block_id(latest).map_err(|e| {
        ErrorObject::owned(ErrorCode::InternalError.code(), e.to_string(), Some(""))
    })?;

    for (address, storage) in known_accounts.iter() {
        match storage {
            AccountStorage::Slots(slots) => {
                for (slot, value) in slots.iter() {
                    let current =
                        state
                            .storage(*address, StorageKey::from(*slot))
                            .map_err(|e| {
                                ErrorObject::owned(
                                    ErrorCode::InternalError.code(),
                                    e.to_string(),
                                    Some(""),
                                )
                            })?;
                    if let Some(current) = current {
                        if FixedBytes::<32>::from_slice(&current.to_be_bytes::<32>()) != *value {
                            return Err(ErrorCode::from(-32003).into());
                        }
                    } else {
                        return Err(ErrorCode::from(-32003).into());
                    }
                }
            }
            AccountStorage::RootHash(expected) => {
                let root = state
                    .storage_root(*address, Default::default())
                    .map_err(|e| {
                        ErrorObject::owned(ErrorCode::InternalError.code(), e.to_string(), Some(""))
                    })?;
                if *expected != root {
                    return Err(ErrorCode::from(-32003).into());
                }
            }
        }
    }
    Ok(())
}
