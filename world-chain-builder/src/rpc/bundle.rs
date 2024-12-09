use crate::{pool::tx::WorldChainPooledTransaction, primitives::recover_raw_transaction};
use alloy_eips::BlockId;
use alloy_primitives::map::HashMap;
use alloy_rpc_types::erc4337::{AccountStorage, ConditionalOptions};
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
    types::{ErrorCode, ErrorObject, ErrorObjectOwned},
};
use reth::transaction_pool::{PoolTransaction, TransactionOrigin, TransactionPool};
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use revm_primitives::{map::FbBuildHasher, Address, Bytes, FixedBytes, B256};

/// Trait interface for `eth_sendRawTransactionConditional`
#[cfg_attr(not(test), rpc(server, namespace = "eth"))]
#[cfg_attr(test, rpc(server, client, namespace = "eth"))]
#[async_trait]
pub trait EthTransactionsExt {
    #[method(name = "sendRawTransactionConditional")]
    async fn send_raw_transaction_conditional(
        &self,
        tx: Bytes,
        options: ConditionalOptions,
    ) -> RpcResult<B256>;
}

/// WorldChainEthApi Extension for ERC-4337 Conditionally Included
///
/// Bundled Transactions
#[derive(Clone, Debug)]
pub struct WorldChainEthApiExt<Pool, Client> {
    pool: Pool,
    client: Client,
}

#[async_trait]
impl<Pool, Client> EthTransactionsExtServer for WorldChainEthApiExt<Pool, Client>
where
    Pool: TransactionPool<Transaction = WorldChainPooledTransaction> + Clone + 'static,
    Client: BlockReaderIdExt + StateProviderFactory + 'static,
{
    async fn send_raw_transaction_conditional(
        &self,
        tx: Bytes,
        options: ConditionalOptions,
    ) -> RpcResult<B256> {
        self.validate_options(options)?;
        let (recovered, _) = recover_raw_transaction(tx.clone())?;
        let pool_transaction = WorldChainPooledTransaction::from_pooled(recovered);

        // submit the transaction to the pool with a `Local` origin
        let hash = self
            .pool()
            .add_transaction(TransactionOrigin::Local, pool_transaction)
            .await
            .map_err(|_| ErrorObjectOwned::from(ErrorCode::InternalError))?;

        Ok(hash)
    }
}

impl<Pool, Client> WorldChainEthApiExt<Pool, Client>
where
    Pool: TransactionPool<Transaction = WorldChainPooledTransaction> + Clone + 'static,
    Client: BlockReaderIdExt + StateProviderFactory + 'static,
{
    pub fn new(pool: Pool, client: Client) -> Self {
        Self { pool, client }
    }

    pub fn provider(&self) -> &Client {
        &self.client
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Validates the conditional inclusion options provided by the client.
    ///
    /// reference for the implementation <https://notes.ethereum.org/@yoav/SkaX2lS9j#>
    /// See also <https://pkg.go.dev/github.com/aK0nshin/go-ethereum/arbitrum_types#ConditionalOptions>
    pub fn validate_options(&self, options: ConditionalOptions) -> RpcResult<()> {
        let latest = self
            .provider()
            .block_by_id(BlockId::latest())
            .map_err(|e| {
                ErrorObject::owned(ErrorCode::InternalError.code(), e.to_string(), Some(""))
            })?
            .ok_or(ErrorObjectOwned::from(ErrorCode::InternalError))?;

        self.validate_known_accounts(options.known_accounts, latest.header.number.into())?;

        if let Some(min_block) = options.block_number_min {
            if min_block > latest.number {
                return Err(ErrorCode::from(-32003).into());
            }
        }

        if let Some(max_block) = options.block_number_max {
            if max_block <= latest.number {
                return Err(ErrorCode::from(-32003).into());
            }
        }

        if let Some(min_timestamp) = options.timestamp_min {
            if min_timestamp > latest.timestamp {
                return Err(ErrorCode::from(-32003).into());
            }
        }

        if let Some(max_timestamp) = options.timestamp_max {
            if max_timestamp <= latest.timestamp {
                return Err(ErrorCode::from(-32003).into());
            }
        }

        Ok(())
    }

    /// Validates the account storage slots/storage root provided by the client
    ///
    /// Matches the current state of the account storage slots/storage root.
    pub fn validate_known_accounts(
        &self,
        known_accounts: HashMap<Address, AccountStorage, FbBuildHasher<20>>,
        latest: BlockId,
    ) -> RpcResult<()> {
        let state = self
            .provider()
            .state_by_block_id(latest)
            .map_err(|e| {
                ErrorObject::owned(ErrorCode::InternalError.code(), e.to_string(), Some(""))
            })?;

        for (address, storage) in known_accounts.into_iter() {
            match storage {
                AccountStorage::Slots(slots) => {
                    for (slot, value) in slots.into_iter() {
                        let current = state.storage(address, slot.into()).map_err(|e| {
                            ErrorObject::owned(
                                ErrorCode::InternalError.code(),
                                e.to_string(),
                                Some(""),
                            )
                        })?;
                        if let Some(current) = current {
                            if FixedBytes::<32>::from_slice(&current.to_be_bytes::<32>()) != value {
                                return Err(ErrorCode::from(-32003).into());
                            }
                        } else {
                            return Err(ErrorCode::from(-32003).into());
                        }
                    }
                }
                AccountStorage::RootHash(expected) => {
                    let root = state
                        .storage_root(address, Default::default())
                        .map_err(|e| {
                            ErrorObject::owned(
                                ErrorCode::InternalError.code(),
                                e.to_string(),
                                Some(""),
                            )
                        })?;
                    if *expected != root {
                        return Err(ErrorCode::from(-32003).into());
                    }
                }
            }
        }
        Ok(())
    }
}
