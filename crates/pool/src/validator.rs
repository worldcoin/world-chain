//! World Chain transaction pool types
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicU64, Ordering},
    },
};

use super::{root::WorldChainRootValidator, tx::WorldChainPoolTransaction};
use crate::{
    bindings::{IPBHEntryPoint, IPBHEntryPoint::PBHPayload},
    error::WorldChainTransactionPoolError,
    tx::WorldChainPoolTransactionError,
};
use alloy_eips::BlockId;
use alloy_primitives::{Address, keccak256};
use alloy_sol_types::{SolCall, SolValue};
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use reth_evm::ConfigureEvm;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_primitives_traits::{
    BlockTy, GotExpected, SealedBlock, TxTy, transaction::error::InvalidTransactionError,
};
use reth_provider::{BlockReaderIdExt, ChainSpecProvider, StateProviderFactory};
use reth_transaction_pool::{
    TransactionOrigin, TransactionValidationOutcome, TransactionValidator,
    validate::ValidTransaction,
};
use revm_primitives::U256;
use tracing::info;
use world_chain_pbh::payload::{PBHPayload as PbhPayload, PBHValidationError};
use world_chain_primitives::transaction::WIP_1001_TX_TYPE;

/// The slot of the `pbh_gas_limit` in the PBHEntryPoint contract.
pub const PBH_GAS_LIMIT_SLOT: U256 = U256::from_limbs([53, 0, 0, 0]);

/// The slot of the `pbh_nonce_limit` in the PBHEntryPoint contract.
pub const PBH_NONCE_LIMIT_SLOT: U256 = U256::from_limbs([50, 0, 0, 0]);

/// The offset in bits of the `PBH_NONCE_LIMIT_SLOT` containing the u16 nonce limit.
pub const PBH_NONCE_LIMIT_OFFSET: u32 = 160;

/// Max u16
pub const MAX_U16: U256 = U256::from_limbs([0xFFFF, 0, 0, 0]);

/// The min validation failure fee that world chain account MUST have for
/// a WIP-1001 tx to be considered valid and inserted into the mempool.
// TODO: change this value to an appropriate one!!!
pub const MIN_VALIDATION_FAILURE_FEE: U256 = U256::from_limbs([0xFFFF, 0, 0, 0]);

// ---------------------------------------------------------------------------
// WIP-1001 `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy storage layout
// ---------------------------------------------------------------------------
//
// The constants below MUST match the storage layout of
// `pkg/contracts/src/WorldChainAccountManager.sol`. That contract is currently
// `TODO: FIXME`. The values here encode the layout we expect once it lands and
// are pinned by a Rust unit test (`storage_layout::tests`).
//
// Expected Solidity layout:
//
//   contract WorldChainAccountManager {
//       mapping(address => Account) internal accounts; // slot 0
//   }
//
//   struct Account {
//       WorldChainAccountVerifier admin;             // slots base + [0..1]
//                                                    //   .verifier  : address       (slot base + 0)
//                                                    //   .installation: bytes header (slot base + 1)
//       bytes32 accountSalt;                         // slot base + 2
//       WorldChainAccountVerifier[] sessionVerifiers;// slot base + 3 (length)
//       bytes32 keyRingHash;                         // slot base + 4
//       uint64 adminNonce;                           // slot base + 5, bits   [0..63]
//       uint64 transactionNonce;                     // slot base + 5, bits  [64..127]
//   }
//
// For a given world-chain account address `a`, the base slot of
// `accounts[a]` is `keccak256(abi.encode(a, ACCOUNTS_MAPPING_SLOT))`.

/// The address of the WIP-1001 `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy.
///
/// Per WIP-1001 "Activation Parameters" this is TBD in the spec; tracking
/// `Address::ZERO` here until the activation parameter is assigned.
// TODO: replace with the canonical predeploy address once WIP-1001 fork
// configuration assigns it.
pub const WORLD_CHAIN_ACCOUNT_MANAGER: Address = Address::ZERO;

/// Storage slot of the `accounts` mapping inside `WorldChainAccountManager`.
// TODO: confirm against the Solidity layout once `WorldChainAccountManager.sol`
// is implemented.
pub const WORLD_CHAIN_ACCOUNTS_MAPPING_SLOT: U256 = U256::ZERO;

/// Offset, in slots, of the slot that packs `adminNonce` and
/// `transactionNonce` within the per-account `Account` struct.
// TODO: confirm against the Solidity layout once `WorldChainAccountManager.sol`
// is implemented.
pub const WORLD_CHAIN_ACCOUNT_NONCE_PACKED_SLOT_OFFSET: u64 = 5;

/// Bit-offset of the `transactionNonce` field within
/// `accounts[a]`'s nonce-packed slot. Solidity packs the first-declared
/// `adminNonce` into the low 64 bits, putting `transactionNonce` at offset 64.
pub const WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET: u32 = 64;

/// Mask for the 64 bits occupied by `transactionNonce` inside its slot.
pub const U64_MASK: U256 = U256::from_limbs([u64::MAX, 0, 0, 0]);

/// Computes the storage slot holding `accounts[world_chain_account].adminNonce`
/// and `.transactionNonce` (both packed in the same slot).
fn world_chain_account_nonce_slot(world_chain_account: Address) -> U256 {
    let key_encoded =
        (world_chain_account, WORLD_CHAIN_ACCOUNTS_MAPPING_SLOT).abi_encode();
    let base_slot = U256::from_be_bytes::<32>(keccak256(&key_encoded).0);
    base_slot + U256::from(WORLD_CHAIN_ACCOUNT_NONCE_PACKED_SLOT_OFFSET)
}

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    /// The inner transaction validator.
    inner: OpTransactionValidator<Client, Tx, Evm>,
    /// Validates World ID proofs contain a valid root in the WorldID account.
    root_validator: WorldChainRootValidator<Client>,
    /// The maximum number of PBH transactions a single World ID can execute in a given month.
    max_pbh_nonce: Arc<AtomicU16>,
    /// The maximum amount of gas a single PBH transaction can consume.
    max_pbh_gas_limit: Arc<AtomicU64>,
    /// The address of the entrypoint for all PBH transactions.
    pbh_entrypoint: Address,
    /// The address of the World ID PBH signature aggregator.
    pbh_signature_aggregator: Address,
}

impl<Client, Tx, Evm> WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: ChainSpecProvider<ChainSpec: OpHardforks>
        + StateProviderFactory
        + BlockReaderIdExt<Block = BlockTy<Evm::Primitives>>,
    Tx: WorldChainPoolTransaction<Consensus = TxTy<Evm::Primitives>>,
    Evm: ConfigureEvm,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(
        inner: OpTransactionValidator<Client, Tx, Evm>,
        root_validator: WorldChainRootValidator<Client>,
        pbh_entrypoint: Address,
        pbh_signature_aggregator: Address,
    ) -> Result<Self, WorldChainTransactionPoolError> {
        let state = inner.client().state_by_block_id(BlockId::latest())?;
        // The `num_pbh_txs` storage is in a packed slot at a 160 bit offset consuming 16 bits.
        let max_pbh_nonce: u16 = ((state
            .storage(pbh_entrypoint, PBH_NONCE_LIMIT_SLOT.into())?
            .unwrap_or_default()
            >> PBH_NONCE_LIMIT_OFFSET)
            & MAX_U16)
            .to();
        let max_pbh_gas_limit: u64 = state
            .storage(pbh_entrypoint, PBH_GAS_LIMIT_SLOT.into())?
            .unwrap_or_default()
            .to();

        if max_pbh_nonce == 0 && max_pbh_gas_limit == 0 {
            info!(
                %pbh_entrypoint,
                %pbh_signature_aggregator,
                "WorldChainTransactionValidator Initialized with PBH Disabled - Failed to fetch PBH nonce and gas limit from PBHEntryPoint. Defaulting to 0."
            )
        } else {
            info!(
                %max_pbh_gas_limit,
                %max_pbh_nonce,
                %pbh_entrypoint,
                %pbh_signature_aggregator,
                "WorldChainTransactionValidator Initialized with PBH Enabled"
            )
        }
        Ok(Self {
            inner,
            root_validator,
            max_pbh_nonce: Arc::new(AtomicU16::new(max_pbh_nonce)),
            max_pbh_gas_limit: Arc::new(AtomicU64::new(max_pbh_gas_limit)),
            pbh_entrypoint,
            pbh_signature_aggregator,
        })
    }

    /// Get a reference to the inner transaction validator.
    pub fn inner(&self) -> &OpTransactionValidator<Client, Tx, Evm> {
        &self.inner
    }

    /// Validates a PBH bundle transaction
    ///
    /// If the transaction is valid marks it for priority inclusion
    pub async fn validate_pbh_bundle(
        &self,
        origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        // Ensure that the tx is a valid OP transaction and return early if invalid
        let mut tx_outcome = self.inner.validate_one(origin, tx.clone()).await;
        if !tx_outcome.is_valid() {
            return tx_outcome;
        }

        // Decode the calldata and check that all UserOp specify the PBH signature aggregator
        let Ok(calldata) = IPBHEntryPoint::handleAggregatedOpsCall::abi_decode(tx.input()) else {
            return WorldChainPoolTransactionError::from(PBHValidationError::InvalidCalldata)
                .to_outcome(tx);
        };

        // Reject empty aggregator arrays - they bypass all validation
        if calldata._0.is_empty() {
            return WorldChainPoolTransactionError::from(PBHValidationError::MissingPbhPayload)
                .to_outcome(tx);
        }

        if !calldata
            ._0
            .iter()
            .all(|aggregator| aggregator.aggregator == self.pbh_signature_aggregator)
        {
            return WorldChainPoolTransactionError::from(
                PBHValidationError::InvalidSignatureAggregator,
            )
            .to_outcome(tx);
        }

        // Validate all proofs associated with each UserOp
        let mut aggregated_payloads = vec![];
        let mut seen_nullifier_hashes = HashSet::new();

        for aggregated_ops in calldata._0 {
            let buff = aggregated_ops.signature.as_ref();
            let pbh_payloads = match <Vec<PBHPayload>>::abi_decode(buff) {
                Ok(pbh_payloads) => pbh_payloads,
                Err(_) => {
                    return WorldChainPoolTransactionError::from(
                        PBHValidationError::InvalidCalldata,
                    )
                    .to_outcome(tx);
                }
            };

            if pbh_payloads.len() != aggregated_ops.userOps.len() {
                return WorldChainPoolTransactionError::from(PBHValidationError::MissingPbhPayload)
                    .to_outcome(tx);
            }

            let valid_roots = self.root_validator.roots();

            let payloads: Vec<PbhPayload> = match pbh_payloads
                .into_par_iter()
                .zip(aggregated_ops.userOps)
                .map(|(payload, op)| {
                    let signal = crate::eip4337::hash_user_op(&op);
                    let Ok(payload) = PbhPayload::try_from(payload) else {
                        return Err(PBHValidationError::InvalidCalldata.into());
                    };
                    payload.validate(
                        signal,
                        &valid_roots,
                        self.max_pbh_nonce.load(Ordering::Relaxed),
                    )?;
                    Ok::<PbhPayload, WorldChainPoolTransactionError>(payload)
                })
                .collect::<Result<Vec<PbhPayload>, WorldChainPoolTransactionError>>()
            {
                Ok(payloads) => payloads,
                Err(err) => return err.to_outcome(tx),
            };

            // Now check for duplicate nullifier_hashes
            for payload in &payloads {
                if !seen_nullifier_hashes.insert(payload.nullifier_hash) {
                    return WorldChainPoolTransactionError::from(
                        PBHValidationError::DuplicateNullifierHash,
                    )
                    .to_outcome(tx);
                }
            }

            aggregated_payloads.extend(payloads);
        }

        if let TransactionValidationOutcome::Valid {
            transaction: ValidTransaction::Valid(tx),
            ..
        } = &mut tx_outcome
        {
            if !aggregated_payloads.is_empty() {
                tx.set_pbh_payloads(aggregated_payloads);
            } else {
                return WorldChainPoolTransactionError::from(PBHValidationError::MissingPbhPayload)
                    .to_outcome(tx.clone());
            }
        }

        tx_outcome
    }

    pub async fn validate_pbh(
        &self,
        origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        if tx.gas_limit() > self.max_pbh_gas_limit.load(Ordering::Relaxed) {
            return WorldChainPoolTransactionError::from(PBHValidationError::PbhGasLimitExceeded)
                .to_outcome(tx);
        }

        let function_signature: [u8; 4] = tx
            .input()
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_default();

        match function_signature {
            IPBHEntryPoint::handleAggregatedOpsCall::SELECTOR => {
                self.validate_pbh_bundle(origin, tx).await
            }
            _ => self.inner.validate_one(origin, tx.clone()).await,
        }
    }

    pub async fn validate_wip1001(
        &self,
        origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        let state = match self.inner.client().latest() {
            Ok(new_state) => Box::new(new_state),
            Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
        };
        // check whether world chain account exists
        // TODO: it's better to call WorldChainAccountManager.getKeyRingHash(account)
        // and ensure it's not zero, therefore we're sure that account is a valid and esistent
        // world chain account.
        let world_chain_account_addr = tx.sender();
        let world_chain_account = match state.basic_account(&world_chain_account_addr) {
            Ok(maybe_account) => match maybe_account {
                Some(account) => account,
                None => {
                    return TransactionValidationOutcome::Error(
                        *tx.hash(),
                        Box::new(
                            WorldChainPoolTransactionError::WorldChainAccountDoesNotExist(
                                world_chain_account_addr,
                            ),
                        ),
                    );
                }
            },
            Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
        };
        // world chain account balance must cover `MIN_VALIDATION_FAILURE_FEE`
        if world_chain_account.balance < MIN_VALIDATION_FAILURE_FEE {
            return TransactionValidationOutcome::Error(
                *tx.hash(),
                InvalidTransactionError::InsufficientFunds(
                    GotExpected {
                        got: world_chain_account.balance,
                        expected: MIN_VALIDATION_FAILURE_FEE,
                    }
                    .into(),
                )
                .into(),
            );
        }
        // tx nonce should be greater or equal to the account.transactionNonce
        let tx_nonce = tx.nonce();
        let nonce_slot = world_chain_account_nonce_slot(world_chain_account_addr);
        let account_tx_nonce: u64 = match state
            .storage(WORLD_CHAIN_ACCOUNT_MANAGER, nonce_slot.into())
        {
            Ok(maybe_slot) => ((maybe_slot.unwrap_or_default()
                >> WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET)
                & U64_MASK)
                .to(),
            Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
        };
        if tx_nonce < account_tx_nonce {
            return WorldChainPoolTransactionError::StaleTransactionNonce {
                got: tx_nonce,
                expected: account_tx_nonce,
            }
            .to_outcome(tx);
        }

        todo!()
    }
}

impl<Client, Tx, Evm> TransactionValidator for WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: ChainSpecProvider<ChainSpec: OpHardforks>
        + StateProviderFactory
        + BlockReaderIdExt<Block = BlockTy<Evm::Primitives>>,
    Tx: WorldChainPoolTransaction<Consensus = TxTy<Evm::Primitives>>,
    Evm: ConfigureEvm,
{
    type Transaction = Tx;

    type Block = BlockTy<Evm::Primitives>;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        if transaction.ty() == WIP_1001_TX_TYPE {
            return self.validate_wip1001(origin, transaction.clone()).await;
        }
        if transaction.to().unwrap_or_default() != self.pbh_entrypoint {
            return self.inner.validate_one(origin, transaction.clone()).await;
        }

        self.validate_pbh(origin, transaction).await
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock<Self::Block>) {
        // Try and fetch the max pbh nonce and gas limit from the state at the latest block
        if let Ok(state) = self.inner.client().state_by_block_id(BlockId::latest()) {
            if let Some(max_pbh_nonce) = state
                .storage(self.pbh_entrypoint, PBH_NONCE_LIMIT_SLOT.into())
                .ok()
                .flatten()
            {
                let max_pbh_nonce = (max_pbh_nonce >> PBH_NONCE_LIMIT_OFFSET) & MAX_U16;
                self.max_pbh_nonce
                    .store(max_pbh_nonce.to(), Ordering::Relaxed);
            }

            if let Some(max_pbh_gas_limit) = state
                .storage(self.pbh_entrypoint, PBH_GAS_LIMIT_SLOT.into())
                .ok()
                .flatten()
            {
                self.max_pbh_gas_limit
                    .store(max_pbh_gas_limit.to(), Ordering::Relaxed);
            }
        }
        self.inner.on_new_head_block(new_tip_block);
        self.root_validator.on_new_block(new_tip_block);
    }
}

#[cfg(test)]
mod storage_layout_tests {
    //! Unit tests pinning the Rust-side derivation of the
    //! `WORLD_CHAIN_ACCOUNT_MANAGER` storage layout. The Solidity contract
    //! (`pkg/contracts/src/WorldChainAccountManager.sol`) is still
    //! `TODO: FIXME`; once it lands, mirror this layout there or update both
    //! sides together.

    use super::*;
    use alloy_primitives::address;

    /// `keccak256(abi.encode(addr, 0))` for a fixed address — pinned vector so
    /// future changes to the slot derivation surface as a test failure.
    #[test]
    fn account_nonce_slot_matches_known_vector() {
        let acc = address!("000000000000000000000000000000000000001d");
        let slot = world_chain_account_nonce_slot(acc);

        // base = keccak256(abi.encode(acc, uint256(0))) computed via the same
        // primitive used in the helper, but spelled out independently here so
        // a regression in the helper would diverge from this hand-rolled
        // expectation.
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(acc.as_slice());
        let base = U256::from_be_bytes::<32>(keccak256(buf).0);
        let expected = base + U256::from(WORLD_CHAIN_ACCOUNT_NONCE_PACKED_SLOT_OFFSET);

        assert_eq!(slot, expected);
    }

    #[test]
    fn account_nonce_slot_is_deterministic_per_address() {
        let a = address!("000000000000000000000000000000000000001d");
        let b = address!("00000000000000000000000000000000000000aa");
        assert_eq!(world_chain_account_nonce_slot(a), world_chain_account_nonce_slot(a));
        assert_ne!(world_chain_account_nonce_slot(a), world_chain_account_nonce_slot(b));
    }

    #[test]
    fn transaction_nonce_extracted_from_packed_slot() {
        // Build a packed slot value: adminNonce = 0x1122334455667788, transactionNonce = 0xaabbccddeeff0011.
        // Solidity packs first-declared into the low bits, so the slot word is
        // (transactionNonce << 64) | adminNonce.
        let admin_nonce: u64 = 0x1122_3344_5566_7788;
        let tx_nonce: u64 = 0xaabb_ccdd_eeff_0011;

        let slot_value =
            (U256::from(tx_nonce) << WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET) | U256::from(admin_nonce);

        let extracted: u64 = ((slot_value >> WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET) & U64_MASK).to();
        assert_eq!(extracted, tx_nonce);

        // Sanity-check the `adminNonce` half too (low 64 bits).
        let admin_extracted: u64 = (slot_value & U64_MASK).to();
        assert_eq!(admin_extracted, admin_nonce);
    }

    #[test]
    fn nonce_packed_slot_layout_constants_are_in_sync() {
        // These constants form a tight contract; pin them so accidental edits
        // require touching the layout doc-block above too.
        assert_eq!(WORLD_CHAIN_ACCOUNT_NONCE_PACKED_SLOT_OFFSET, 5);
        assert_eq!(WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET, 64);
        assert_eq!(U64_MASK, U256::from(u64::MAX));
        assert_eq!(WORLD_CHAIN_ACCOUNTS_MAPPING_SLOT, U256::ZERO);
        assert_eq!(WORLD_CHAIN_ACCOUNT_MANAGER, Address::ZERO);
    }
}

#[cfg(test)]
pub mod tests {
    use alloy_consensus::{Block, BlockBody, Header};
    use alloy_primitives::{Address, address};
    use alloy_sol_types::SolCall;
    use reth_optimism_primitives::OpTransactionSigned;
    use reth_primitives_traits::SealedBlock;
    use reth_transaction_pool::{
        Pool, TransactionPool, TransactionValidator, blobstore::InMemoryBlobStore,
    };
    use world_chain_evm::WorldChainEvmConfig;
    use world_chain_pbh::{date_marker::DateMarker, external_nullifier::ExternalNullifier};
    use world_chain_test_utils::{
        PBH_DEV_ENTRYPOINT,
        utils::{TREE, account, eip1559, eth_tx, pbh_bundle, pbh_multicall, user_op},
    };

    /// Devnet World ID for testing
    const DEV_WORLD_ID: Address = address!("5FbDB2315678afecb367f032d93F642f64180aa3");

    use crate::{
        ordering::WorldChainOrdering, root::LATEST_ROOT_SLOT, tx::WorldChainPooledTransaction,
    };
    use world_chain_test_utils::mock::{ExtendedAccount, MockEthProvider};

    use super::WorldChainTransactionValidator;

    /// Test constants
    const PBH_DEV_SIGNATURE_AGGREGATOR: Address =
        address!("Cf7Ed3AccA5a467e9e704C703E8D87F634fB0Fc9");

    type TestValidator = WorldChainTransactionValidator<
        MockEthProvider,
        WorldChainPooledTransaction,
        WorldChainEvmConfig,
    >;

    /// Create a World Chain validator for testing
    fn world_chain_validator() -> TestValidator {
        use super::{MAX_U16, PBH_GAS_LIMIT_SLOT, PBH_NONCE_LIMIT_SLOT};
        use crate::root::WorldChainRootValidator;
        use reth_optimism_node::txpool::OpTransactionValidator;
        use reth_transaction_pool::{
            blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
        };
        use revm_primitives::U256;

        let client = MockEthProvider::default();
        let header = Header::default();
        let hash = header.hash_slow();
        client.add_block(
            hash,
            Block {
                header,
                body: Default::default(),
            },
        );
        let evm = WorldChainEvmConfig::optimism(client.chain_spec.clone());

        let validator = EthTransactionValidatorBuilder::new(client.clone(), evm)
            .no_shanghai()
            .no_cancun()
            .build(InMemoryBlobStore::default());
        let validator = OpTransactionValidator::new(validator).require_l1_data_gas_fee(false);
        let root_validator = WorldChainRootValidator::new(client, DEV_WORLD_ID).unwrap();
        validator.client().add_account(
            PBH_DEV_ENTRYPOINT,
            ExtendedAccount::new(0, alloy_primitives::U256::ZERO).extend_storage(vec![
                (PBH_GAS_LIMIT_SLOT.into(), U256::from(15000000)),
                (
                    PBH_NONCE_LIMIT_SLOT.into(),
                    ((MAX_U16 - U256::from(1)) << U256::from(160)),
                ),
            ]),
        );
        WorldChainTransactionValidator::new(
            validator,
            root_validator,
            PBH_DEV_ENTRYPOINT,
            PBH_DEV_SIGNATURE_AGGREGATOR,
        )
        .expect("failed to create world chain validator")
    }

    async fn setup()
    -> Pool<TestValidator, WorldChainOrdering<WorldChainPooledTransaction>, InMemoryBlobStore> {
        let validator = world_chain_validator();

        // Fund 10 test accounts
        for acc in 0..10 {
            let account_address = account(acc);

            validator.inner().client().add_account(
                account_address,
                ExtendedAccount::new(0, alloy_primitives::U256::MAX),
            );
        }

        // Prep a merkle tree with first 5 accounts
        let root = TREE.root();

        // Insert a world id root into the OpWorldId Account
        validator.inner().client().add_account(
            DEV_WORLD_ID,
            ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
                .extend_storage(vec![(LATEST_ROOT_SLOT.into(), root)]),
        );

        let header = Header {
            gas_limit: 20000000,
            ..Default::default()
        };
        let body = BlockBody::<OpTransactionSigned>::default();
        let block = SealedBlock::seal_slow(Block { header, body });

        // Propogate the block to the root validator
        validator.on_new_head_block(&block);

        let ordering = WorldChainOrdering::default();

        Pool::new(
            validator,
            ordering,
            InMemoryBlobStore::default(),
            Default::default(),
        )
    }

    #[tokio::test]
    async fn validate_noop_non_pbh() {
        const ACC: u32 = 0;

        let pool = setup().await;

        let account = account(ACC);
        let tx = eip1559().to(account).call();
        let tx = eth_tx(ACC, tx).await;

        pool.add_external_transaction(tx.into())
            .await
            .expect("Failed to add transaction");
    }

    #[tokio::test]
    async fn validate_no_duplicates() {
        const ACC: u32 = 0;

        let pool = setup().await;

        let account = account(ACC);
        let tx = eip1559().to(account).call();
        let tx = eth_tx(ACC, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect("Failed to add transaction");

        let res = pool.add_external_transaction(tx.into()).await;

        assert!(res.is_err());
    }

    #[tokio::test]
    async fn validate_pbh_bundle() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();
        let bundle = pbh_bundle(vec![user_op], vec![proof.into()]);
        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect("Failed to add transaction");
    }

    #[tokio::test]
    async fn validate_pbh_bundle_duplicate_nullifier_hash() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();

        // Lets add two of the same userOp in the bundle so the nullifier hash is the same and we should expect an error
        let bundle = pbh_bundle(
            vec![user_op.clone(), user_op],
            vec![proof.clone().into(), proof.into()],
        );
        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let res = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Failed to add transaction");

        assert!(res.to_string().contains("Duplicate nullifier hash"),);
    }

    #[tokio::test]
    async fn validate_bundle_no_pbh() {
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        // NOTE: We're ignoring the proof here
        let (user_op, _proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(Address::random()).input(calldata).call();

        let tx = eth_tx(USER_ACCOUNT, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect(
                "Validation should succeed - PBH data is invalid, but this is not a PBH bundle",
            );
    }

    #[tokio::test]
    async fn validate_pbh_bundle_missing_proof_for_user_op() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        // NOTE: We're ignoring the proof here
        let (user_op, _proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err.to_string().contains("Missing PBH Payload"),);
    }

    #[tokio::test]
    async fn validate_pbh_multicall() {
        const USER_ACCOUNT: u32 = 1;

        let pool = setup().await;

        let calldata = pbh_multicall()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();
        let calldata = calldata.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(USER_ACCOUNT, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect("Failed to add PBH multicall transaction");
    }

    #[tokio::test]
    async fn validate_date_marker_outdated() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let now = chrono::Utc::now();
        let month_in_the_past = now - chrono::Months::new(1);

        // NOTE: We're ignoring the proof here
        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(month_in_the_past),
                0,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![proof.into()]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");
        assert!(
            err.to_string()
                .contains("Invalid external nullifier period"),
        );
    }

    #[tokio::test]
    async fn validate_date_marker_in_the_future() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let now = chrono::Utc::now();
        let month_in_the_future = now + chrono::Months::new(1);

        // NOTE: We're ignoring the proof here
        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(month_in_the_future),
                0,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![proof.into()]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(
            err.to_string()
                .contains("Invalid external nullifier period"),
        );
    }

    #[tokio::test]
    async fn invalid_external_nullifier_nonce() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                u16::MAX,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![proof.into()]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err.to_string().contains("Invalid external nullifier nonce"),);
    }
}
