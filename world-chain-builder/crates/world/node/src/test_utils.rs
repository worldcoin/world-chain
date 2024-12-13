use alloy_eips::{BlockHashOrNumber, BlockId, BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{
    map::{HashMap, HashSet},
    Address, BlockHash, BlockNumber, Bytes, StorageKey, StorageValue, TxHash, TxNumber, B256, U256,
};
use alloy_rpc_types::{Withdrawal, Withdrawals};
use futures::future::join_all;
use reth::api::ConfigureEvmEnv;
use reth::chainspec::{ChainInfo, MAINNET};
use reth::revm::primitives::{BlockEnv, CfgEnvWithHandlerCfg};
use reth::transaction_pool::{
    validate::ValidTransaction, TransactionOrigin, TransactionValidationOutcome,
    TransactionValidator,
};
use reth_chain_state::{
    CanonStateNotifications, CanonStateSubscriptions, ForkChoiceNotifications,
    ForkChoiceSubscriptions,
};
use reth_db::models::{AccountBeforeTx, StoredBlockBodyIndices};
use reth_optimism_chainspec::OpChainSpec;
use reth_primitives::{
    Account, Block, BlockWithSenders, Bytecode, EthPrimitives, Header, Receipt, SealedBlock,
    SealedBlockWithSenders, SealedHeader, TransactionMeta, TransactionSigned,
};
use reth_provider::{
    providers::StaticFileProvider, AccountReader, BlockHashReader, BlockIdReader, BlockNumReader,
    BlockReader, BlockReaderIdExt, BlockSource, ChainSpecProvider, ChangeSetReader, EvmEnvProvider,
    HashedPostStateProvider, HeaderProvider, NodePrimitivesProvider, ProviderError, ProviderResult,
    PruneCheckpointReader, ReceiptProvider, ReceiptProviderIdExt, StateProofProvider,
    StateProvider, StateProviderBox, StateProviderFactory, StateRootProvider,
    StaticFileProviderFactory, StorageRootProvider, TransactionVariant, TransactionsProvider,
    WithdrawalsProvider,
};
use reth_prune_types::{PruneCheckpoint, PruneSegment};
use reth_trie::{
    updates::TrieUpdates, AccountProof, HashedPostState, HashedStorage, MultiProof,
    StorageMultiProof, StorageProof, TrieInput,
};
use std::{
    ops::{RangeBounds, RangeInclusive},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::{broadcast, watch};

use crate::pool::{
    tx::{WorldChainPoolTransaction, WorldChainPooledTransaction},
    validator::WorldChainTransactionValidator,
};

/// Supports various api interfaces for testing purposes.
#[derive(Debug, Clone, Default, Copy)]
#[non_exhaustive]
pub struct WorldChainNoopProvider;

impl ChainSpecProvider for WorldChainNoopProvider {
    type ChainSpec = OpChainSpec;

    fn chain_spec(&self) -> Arc<OpChainSpec> {
        let inner = MAINNET.clone().as_ref().to_owned();
        Arc::new(OpChainSpec::new(inner))
    }
}

/// Noop implementation for testing purposes
impl BlockHashReader for WorldChainNoopProvider {
    fn block_hash(&self, _number: u64) -> ProviderResult<Option<B256>> {
        Ok(None)
    }

    fn canonical_hashes_range(
        &self,
        _start: BlockNumber,
        _end: BlockNumber,
    ) -> ProviderResult<Vec<B256>> {
        Ok(vec![])
    }
}

impl BlockNumReader for WorldChainNoopProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo::default())
    }

    fn best_block_number(&self) -> ProviderResult<BlockNumber> {
        Ok(0)
    }

    fn last_block_number(&self) -> ProviderResult<BlockNumber> {
        Ok(0)
    }

    fn block_number(&self, _hash: B256) -> ProviderResult<Option<BlockNumber>> {
        Ok(None)
    }
}

impl BlockReader for WorldChainNoopProvider {
    type Block = Block;
    fn find_block_by_hash(
        &self,
        hash: B256,
        _source: BlockSource,
    ) -> ProviderResult<Option<Block>> {
        self.block(hash.into())
    }

    fn block(&self, _id: BlockHashOrNumber) -> ProviderResult<Option<Block>> {
        Ok(None)
    }

    fn pending_block(&self) -> ProviderResult<Option<SealedBlock>> {
        Ok(None)
    }

    fn pending_block_with_senders(&self) -> ProviderResult<Option<SealedBlockWithSenders>> {
        Ok(None)
    }

    fn pending_block_and_receipts(&self) -> ProviderResult<Option<(SealedBlock, Vec<Receipt>)>> {
        Ok(None)
    }

    fn ommers(&self, _id: BlockHashOrNumber) -> ProviderResult<Option<Vec<Header>>> {
        Ok(None)
    }

    fn block_body_indices(&self, _num: u64) -> ProviderResult<Option<StoredBlockBodyIndices>> {
        Ok(None)
    }

    fn block_with_senders(
        &self,
        _id: BlockHashOrNumber,
        _transaction_kind: TransactionVariant,
    ) -> ProviderResult<Option<reth_primitives::BlockWithSenders>> {
        Ok(None)
    }

    fn sealed_block_with_senders(
        &self,
        _id: BlockHashOrNumber,
        _transaction_kind: TransactionVariant,
    ) -> ProviderResult<Option<SealedBlockWithSenders>> {
        Ok(None)
    }

    fn block_range(&self, _range: RangeInclusive<BlockNumber>) -> ProviderResult<Vec<Block>> {
        Ok(vec![])
    }

    fn block_with_senders_range(
        &self,
        _range: RangeInclusive<BlockNumber>,
    ) -> ProviderResult<Vec<BlockWithSenders>> {
        Ok(vec![])
    }

    fn sealed_block_with_senders_range(
        &self,
        _range: RangeInclusive<BlockNumber>,
    ) -> ProviderResult<Vec<SealedBlockWithSenders>> {
        Ok(vec![])
    }

    fn block_by_hash(&self, _hash: B256) -> ProviderResult<Option<Self::Block>> {
        Ok(None)
    }

    fn block_by_number(&self, _num: u64) -> ProviderResult<Option<Self::Block>> {
        Ok(None)
    }
}

impl BlockReaderIdExt for WorldChainNoopProvider {
    fn block_by_id(&self, _id: BlockId) -> ProviderResult<Option<Block>> {
        Ok(None)
    }

    fn sealed_header_by_id(&self, _id: BlockId) -> ProviderResult<Option<SealedHeader>> {
        Ok(None)
    }

    fn header_by_id(&self, _id: BlockId) -> ProviderResult<Option<Header>> {
        Ok(None)
    }

    fn ommers_by_id(&self, _id: BlockId) -> ProviderResult<Option<Vec<Header>>> {
        Ok(None)
    }
}

impl BlockIdReader for WorldChainNoopProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }
}

impl TransactionsProvider for WorldChainNoopProvider {
    type Transaction = TransactionSigned;

    fn transaction_by_id_unhashed(
        &self,
        _id: TxNumber,
    ) -> ProviderResult<Option<Self::Transaction>> {
        Ok(None)
    }

    fn transaction_id(&self, _tx_hash: TxHash) -> ProviderResult<Option<TxNumber>> {
        Ok(None)
    }

    fn transaction_by_id(&self, _id: TxNumber) -> ProviderResult<Option<TransactionSigned>> {
        Ok(None)
    }

    fn transaction_by_hash(&self, _hash: TxHash) -> ProviderResult<Option<TransactionSigned>> {
        Ok(None)
    }

    fn transaction_by_hash_with_meta(
        &self,
        _hash: TxHash,
    ) -> ProviderResult<Option<(TransactionSigned, TransactionMeta)>> {
        Ok(None)
    }

    fn transaction_block(&self, _id: TxNumber) -> ProviderResult<Option<BlockNumber>> {
        Ok(None)
    }

    fn transactions_by_block(
        &self,
        _block_id: BlockHashOrNumber,
    ) -> ProviderResult<Option<Vec<TransactionSigned>>> {
        Ok(None)
    }

    fn transactions_by_block_range(
        &self,
        _range: impl RangeBounds<BlockNumber>,
    ) -> ProviderResult<Vec<Vec<TransactionSigned>>> {
        Ok(Vec::default())
    }

    fn transactions_by_tx_range(
        &self,
        _range: impl RangeBounds<TxNumber>,
    ) -> ProviderResult<Vec<Self::Transaction>> {
        Ok(Vec::default())
    }

    fn senders_by_tx_range(
        &self,
        _range: impl RangeBounds<TxNumber>,
    ) -> ProviderResult<Vec<Address>> {
        Ok(Vec::default())
    }

    fn transaction_sender(&self, _id: TxNumber) -> ProviderResult<Option<Address>> {
        Ok(None)
    }
}

impl ReceiptProvider for WorldChainNoopProvider {
    type Receipt = Receipt;
    fn receipt(&self, _id: TxNumber) -> ProviderResult<Option<Receipt>> {
        Ok(None)
    }

    fn receipt_by_hash(&self, _hash: TxHash) -> ProviderResult<Option<Receipt>> {
        Ok(None)
    }

    fn receipts_by_block(&self, _block: BlockHashOrNumber) -> ProviderResult<Option<Vec<Receipt>>> {
        Ok(None)
    }

    fn receipts_by_tx_range(
        &self,
        _range: impl RangeBounds<TxNumber>,
    ) -> ProviderResult<Vec<Receipt>> {
        Ok(vec![])
    }
}

impl ReceiptProviderIdExt for WorldChainNoopProvider {}

impl HeaderProvider for WorldChainNoopProvider {
    type Header = Header;

    fn header(&self, _block_hash: &BlockHash) -> ProviderResult<Option<Header>> {
        Ok(None)
    }

    fn header_by_number(&self, _num: u64) -> ProviderResult<Option<Header>> {
        Ok(None)
    }

    fn header_td(&self, _hash: &BlockHash) -> ProviderResult<Option<U256>> {
        Ok(None)
    }

    fn header_td_by_number(&self, _number: BlockNumber) -> ProviderResult<Option<U256>> {
        Ok(None)
    }

    fn headers_range(&self, _range: impl RangeBounds<BlockNumber>) -> ProviderResult<Vec<Header>> {
        Ok(vec![])
    }

    fn sealed_header(&self, _number: BlockNumber) -> ProviderResult<Option<SealedHeader>> {
        Ok(None)
    }

    fn sealed_headers_while(
        &self,
        _range: impl RangeBounds<BlockNumber>,
        _predicate: impl FnMut(&SealedHeader) -> bool,
    ) -> ProviderResult<Vec<SealedHeader>> {
        Ok(vec![])
    }
}

impl AccountReader for WorldChainNoopProvider {
    fn basic_account(&self, _address: Address) -> ProviderResult<Option<Account>> {
        Ok(None)
    }
}

impl ChangeSetReader for WorldChainNoopProvider {
    fn account_block_changeset(
        &self,
        _block_number: BlockNumber,
    ) -> ProviderResult<Vec<AccountBeforeTx>> {
        Ok(Vec::default())
    }
}

impl StateRootProvider for WorldChainNoopProvider {
    fn state_root(&self, _state: HashedPostState) -> ProviderResult<B256> {
        Ok(B256::default())
    }

    fn state_root_from_nodes(&self, _input: TrieInput) -> ProviderResult<B256> {
        Ok(B256::default())
    }

    fn state_root_with_updates(
        &self,
        _state: HashedPostState,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((B256::default(), TrieUpdates::default()))
    }

    fn state_root_from_nodes_with_updates(
        &self,
        _input: TrieInput,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((B256::default(), TrieUpdates::default()))
    }
}

impl StorageRootProvider for WorldChainNoopProvider {
    fn storage_multiproof(
        &self,
        _address: Address,
        _slots: &[B256],
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        Ok(StorageMultiProof::empty())
    }

    fn storage_root(
        &self,
        _address: Address,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<B256> {
        Ok(B256::default())
    }

    fn storage_proof(
        &self,
        _address: Address,
        _slot: B256,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageProof> {
        Ok(StorageProof::default())
    }
}

impl StateProofProvider for WorldChainNoopProvider {
    fn proof(
        &self,
        _input: TrieInput,
        address: Address,
        _slots: &[B256],
    ) -> ProviderResult<AccountProof> {
        Ok(AccountProof::new(address))
    }

    fn multiproof(
        &self,
        _input: TrieInput,
        _targets: HashMap<B256, HashSet<B256>>,
    ) -> ProviderResult<MultiProof> {
        Ok(MultiProof::default())
    }

    fn witness(
        &self,
        _input: TrieInput,
        _target: HashedPostState,
    ) -> ProviderResult<HashMap<B256, Bytes>> {
        Ok(HashMap::default())
    }
}

impl StateProvider for WorldChainNoopProvider {
    fn storage(
        &self,
        _account: Address,
        _storage_key: StorageKey,
    ) -> ProviderResult<Option<StorageValue>> {
        Ok(None)
    }

    fn bytecode_by_hash(&self, _code_hash: B256) -> ProviderResult<Option<Bytecode>> {
        Ok(None)
    }
}

impl HashedPostStateProvider for WorldChainNoopProvider {
    fn hashed_post_state(&self, _bundle_state: &reth::revm::db::BundleState) -> HashedPostState {
        HashedPostState::default()
    }
}

impl EvmEnvProvider for WorldChainNoopProvider {
    fn fill_env_with_header<EvmConfig>(
        &self,
        _cfg: &mut CfgEnvWithHandlerCfg,
        _block_env: &mut BlockEnv,
        _header: &Header,
        _evm_config: EvmConfig,
    ) -> ProviderResult<()>
    where
        EvmConfig: ConfigureEvmEnv<Header = Header>,
    {
        Ok(())
    }

    fn fill_cfg_env_at<EvmConfig>(
        &self,
        _cfg: &mut CfgEnvWithHandlerCfg,
        _at: BlockHashOrNumber,
        _evm_config: EvmConfig,
    ) -> ProviderResult<()>
    where
        EvmConfig: ConfigureEvmEnv<Header = Header>,
    {
        Ok(())
    }

    fn fill_cfg_env_with_header<EvmConfig>(
        &self,
        _cfg: &mut CfgEnvWithHandlerCfg,
        _header: &Header,
        _evm_config: EvmConfig,
    ) -> ProviderResult<()>
    where
        EvmConfig: ConfigureEvmEnv<Header = Header>,
    {
        Ok(())
    }
}

impl StateProviderFactory for WorldChainNoopProvider {
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(*self))
    }

    fn history_by_block_number(&self, _block: BlockNumber) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(*self))
    }

    fn history_by_block_hash(&self, _block: BlockHash) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(*self))
    }

    fn state_by_block_hash(&self, _block: BlockHash) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(*self))
    }

    fn pending(&self) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(*self))
    }

    fn state_by_block_number_or_tag(
        &self,
        number_or_tag: BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        match number_or_tag {
            BlockNumberOrTag::Latest => self.latest(),
            BlockNumberOrTag::Finalized => {
                // we can only get the finalized state by hash, not by num
                let hash = self
                    .finalized_block_hash()?
                    .ok_or(ProviderError::FinalizedBlockNotFound)?;

                // only look at historical state
                self.history_by_block_hash(hash)
            }
            BlockNumberOrTag::Safe => {
                // we can only get the safe state by hash, not by num
                let hash = self
                    .safe_block_hash()?
                    .ok_or(ProviderError::SafeBlockNotFound)?;

                self.history_by_block_hash(hash)
            }
            BlockNumberOrTag::Earliest => self.history_by_block_number(0),
            BlockNumberOrTag::Pending => self.pending(),
            BlockNumberOrTag::Number(num) => self.history_by_block_number(num),
        }
    }

    fn pending_state_by_hash(&self, _block_hash: B256) -> ProviderResult<Option<StateProviderBox>> {
        Ok(Some(Box::new(*self)))
    }
}

impl WithdrawalsProvider for WorldChainNoopProvider {
    fn withdrawals_by_block(
        &self,
        _id: BlockHashOrNumber,
        _timestamp: u64,
    ) -> ProviderResult<Option<Withdrawals>> {
        Ok(None)
    }
    fn latest_withdrawal(&self) -> ProviderResult<Option<Withdrawal>> {
        Ok(None)
    }
}

impl PruneCheckpointReader for WorldChainNoopProvider {
    fn get_prune_checkpoint(
        &self,
        _segment: PruneSegment,
    ) -> ProviderResult<Option<PruneCheckpoint>> {
        Ok(None)
    }

    fn get_prune_checkpoints(&self) -> ProviderResult<Vec<(PruneSegment, PruneCheckpoint)>> {
        Ok(Vec::new())
    }
}

impl NodePrimitivesProvider for WorldChainNoopProvider {
    type Primitives = EthPrimitives;
}
impl StaticFileProviderFactory for WorldChainNoopProvider {
    fn static_file_provider(&self) -> StaticFileProvider<Self::Primitives> {
        StaticFileProvider::read_only(PathBuf::default(), false).unwrap()
    }
}

impl CanonStateSubscriptions for WorldChainNoopProvider {
    fn subscribe_to_canonical_state(&self) -> CanonStateNotifications {
        broadcast::channel(1).1
    }
}

impl ForkChoiceSubscriptions for WorldChainNoopProvider {
    type Header = Header;
    fn subscribe_safe_block(&self) -> ForkChoiceNotifications {
        let (_, rx) = watch::channel(None);
        ForkChoiceNotifications(rx)
    }

    fn subscribe_finalized_block(&self) -> ForkChoiceNotifications {
        let (_, rx) = watch::channel(None);
        ForkChoiceNotifications(rx)
    }
}

pub struct WorldChainNoopValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    inner: WorldChainTransactionValidator<Client, Tx>,
}

impl WorldChainNoopValidator<WorldChainNoopProvider, WorldChainPooledTransaction> {
    pub fn new(
        inner: WorldChainTransactionValidator<WorldChainNoopProvider, WorldChainPooledTransaction>,
    ) -> Self {
        Self { inner }
    }
}

impl<Client, Tx> TransactionValidator for WorldChainNoopValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: WorldChainPoolTransaction,
{
    type Transaction = Tx;

    async fn validate_transaction(
        &self,
        _origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        TransactionValidationOutcome::Valid {
            balance: U256::ZERO,
            state_nonce: 0,
            transaction: ValidTransaction::Valid(transaction),
            propagate: false,
        }
    }

    async fn validate_transactions(
        &self,
        transactions: Vec<(TransactionOrigin, Self::Transaction)>,
    ) -> Vec<TransactionValidationOutcome<Self::Transaction>> {
        let futures = transactions
            .into_iter()
            .map(|(origin, tx)| self.validate_transaction(origin, tx))
            .collect::<Vec<_>>();

        join_all(futures).await
    }

    fn on_new_head_block(&self, _new_tip_block: &SealedBlock) {
        unreachable!("NoopValidator does not implement on_new_head_block");
    }
}
