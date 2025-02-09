use std::error::Error;

use alloy_consensus::BlockHeader;
use alloy_eips::BlockId;
use alloy_primitives::{map::HashMap, StorageKey};
use alloy_rpc_types::erc4337::{AccountStorage, TransactionConditional};
use jsonrpsee::{
    core::{async_trait, RpcResult},
    types::{ErrorCode, ErrorObject, ErrorObjectOwned},
};
use reth::{
    api::Block,
    rpc::{
        api::eth::{AsEthApiError, FromEthApiError, FromEvmError},
        server_types::eth::{utils::recover_raw_transaction, EthApiError},
    },
    transaction_pool::{PoolTransaction, TransactionOrigin, TransactionPool},
};
use reth_optimism_node::txpool::OpPooledTransaction;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use revm_primitives::{map::FbBuildHasher, Address, Bytes, FixedBytes, B256};
use world_chain_builder_pool::tx::WorldChainPooledTransaction;

use crate::{core::WorldChainEthApiExt, sequencer::SequencerClient};

#[async_trait]
pub trait EthTransactionsExt {
    /// Extension of [`FromEthApiError`], with network specific errors.
    type Error: Into<jsonrpsee_types::error::ErrorObject<'static>>
        + FromEthApiError
        + AsEthApiError
        // + FromEvmError
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
