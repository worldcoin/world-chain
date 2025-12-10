use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, U256};
use dashmap::DashMap;
use flashblocks_primitives::access_list::FlashblockAccessList;
use rayon::prelude::*;

use revm::state::Bytecode;
use std::collections::{HashMap, HashSet};

pub(crate) type BlockAccessIndex = u16;

/// A convenience builder type for [`FlashblockAccessList`]
#[derive(Debug, Clone)]
pub struct FlashblockAccessListConstruction {
    /// Map from Address -> AccountChangesConstruction
    pub changes: DashMap<Address, AccountChangesConstruction>,
}

impl Default for FlashblockAccessListConstruction {
    fn default() -> Self {
        Self::new()
    }
}

impl FlashblockAccessListConstruction {
    /// Creates a new empty [`FlashblockAccessListConstruction`]
    pub fn new() -> Self {
        Self {
            changes: DashMap::new(),
        }
    }

    /// Merges another [`FlashblockAccessListConstruction`] into this one
    pub fn merge(&mut self, other: Self) {
        for entry in other.changes.into_iter() {
            let (address, other_account_changes) = entry;
            self.changes
                .entry(address)
                .and_modify(|existing| existing.merge(other_account_changes.clone()))
                .or_insert(other_account_changes);
        }
    }

    /// Consumes the builder and produces a [`FlashblockAccessList`]
    pub fn build(self, (min_tx_index, max_tx_index): (u16, u16)) -> FlashblockAccessList {
        // Sort addresses lexicographically
        let mut changes: Vec<_> = self
            .changes
            .into_par_iter()
            .map(|(k, v)| v.build(k))
            .collect();

        changes.par_sort_unstable_by_key(|a| a.address);

        let mut access_list = FlashblockAccessList {
            changes,
            min_tx_index,
            max_tx_index,
        };

        access_list.dedup();
        access_list
    }

    /// Maps a mutable reference to the [`AccountChangesConstruction`] corresponding to `address` at the given closure.
    pub fn map_account_change<F>(&self, address: Address, f: F)
    where
        F: FnOnce(&mut AccountChangesConstruction),
    {
        let mut entry = self
            .changes
            .entry(address)
            .or_insert_with(AccountChangesConstruction::default);

        f(&mut entry);
    }
}

/// A convenience builder type for [`AccountChanges`]
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct AccountChangesConstruction {
    /// Map from Storage Slot -> (Map from transaction index -> Value)
    pub storage_changes: HashMap<U256, HashMap<BlockAccessIndex, U256>>,
    /// Set of storage slots read
    pub storage_reads: HashSet<U256>,
    /// Map of balance changes
    pub balance_changes: HashMap<BlockAccessIndex, U256>,
    /// Map of nonce changes
    pub nonce_changes: HashMap<BlockAccessIndex, u64>,
    /// Map of code changes
    pub code_changes: HashMap<BlockAccessIndex, Bytecode>,
}

impl AccountChangesConstruction {
    /// Merges another [`AccountChangesConstruction`] into this one
    pub fn merge(&mut self, other: Self) {
        for (slot, other_tx_map) in other.storage_changes {
            self.storage_changes
                .entry(slot)
                .and_modify(|existing_tx_map| existing_tx_map.extend(other_tx_map.clone()))
                .or_insert(other_tx_map);
        }
        self.storage_reads.extend(other.storage_reads);
        self.balance_changes.extend(other.balance_changes);
        self.nonce_changes.extend(other.nonce_changes);
        self.code_changes.extend(other.code_changes);
    }

    /// Consumes the builder and produces an [`AccountChanges`] for the given address
    ///
    /// Note: This will sort all changes by transaction index, and storage slots by their value.
    pub fn build(mut self, address: Address) -> AccountChanges {
        let sorted_storage_changes: Vec<_> = {
            let mut slots = self.storage_changes.keys().cloned().collect::<Vec<_>>();
            slots.sort_unstable();

            slots
                .into_iter()
                .map(|s| {
                    let mut storage_changes = self
                        .storage_changes
                        .remove(&s)
                        .unwrap()
                        .into_iter()
                        .collect::<Vec<_>>();

                    storage_changes.sort_unstable_by_key(|(tx_index, _)| *tx_index);

                    (s, storage_changes)
                })
                .collect()
        };

        let mut storage_reads_sorted: Vec<_> = self.storage_reads.drain().collect();
        storage_reads_sorted.sort_unstable();

        let mut balance_changes_sorted: Vec<_> = self.balance_changes.drain().collect();
        balance_changes_sorted.sort_unstable_by_key(|(tx_index, _)| *tx_index);

        let mut nonce_changes_sorted: Vec<_> = self.nonce_changes.drain().collect();
        nonce_changes_sorted.sort_unstable_by_key(|(tx_index, _)| *tx_index);

        let mut code_changes_sorted: Vec<_> = self.code_changes.drain().collect();
        code_changes_sorted.sort_unstable_by_key(|(tx_index, _)| *tx_index);

        AccountChanges {
            address,
            storage_changes: sorted_storage_changes
                .into_iter()
                .map(|(slot, tx_map)| SlotChanges {
                    slot: slot.into(),
                    changes: tx_map
                        .into_iter()
                        .map(|(tx_index, value)| StorageChange {
                            block_access_index: tx_index as u64,
                            new_value: value.into(),
                        })
                        .collect(),
                })
                .collect(),
            storage_reads: storage_reads_sorted.into_iter().map(Into::into).collect(),
            balance_changes: balance_changes_sorted
                .into_iter()
                .map(|(tx_index, value)| BalanceChange {
                    block_access_index: tx_index as u64,
                    post_balance: value,
                })
                .collect(),
            nonce_changes: nonce_changes_sorted
                .into_iter()
                .map(|(tx_index, value)| NonceChange {
                    block_access_index: tx_index as u64,
                    new_nonce: value,
                })
                .collect(),
            code_changes: code_changes_sorted
                .into_iter()
                .map(|(tx_index, bytecode)| CodeChange {
                    block_access_index: tx_index as u64,
                    new_code: bytecode.original_bytes(),
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.storage_changes.is_empty()
            && self.storage_reads.is_empty()
            && self.balance_changes.is_empty()
            && self.nonce_changes.is_empty()
            && self.code_changes.is_empty()
    }
}

// #[cfg(test)]
// mod tests {
//     use std::collections::HashSet;

//     use crate::executor::bal_builder::AsyncBalBuilderBlockExecutor;
//     use alloy_consensus::{TxEip1559, constants::KECCAK_EMPTY};
//     use alloy_eip7928::{AccountChanges, BalanceChange, CodeChange, NonceChange};
//     use alloy_genesis::{Genesis, GenesisAccount};
//     use alloy_op_evm::{OpBlockExecutionCtx, OpEvmFactory};
//     use alloy_primitives::{Address, Bytes, FixedBytes, TxKind, U256, address, bytes, keccak256};

//     use alloy_signer_local::PrivateKeySigner;
//     use alloy_sol_types::{SolCall, sol};
//     use flashblocks_primitives::access_list::FlashblockAccessList;
//     use lazy_static::lazy_static;
//     use op_alloy_consensus::{OpTxEnvelope, OpTypedTransaction};
//     use op_alloy_network::TxSignerSync;
//     use reth::revm::State;
//     use reth_evm::{ConfigureEvm, Evm, EvmFactory, block::BlockExecutor};
//     use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
//     use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
//     use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
//     use reth_primitives::{Recovered, transaction::SignedTransaction};
//     use reth_provider::BlockExecutionResult;
//     use revm::{
//         database::{
//             BundleAccount, BundleState, InMemoryDB,
//             states::{bundle_state::BundleRetention, reverts::Reverts},
//         },
//         state::{AccountInfo, Bytecode},
//     };

//     sol! {
//         #[sol(bytecode = "0x6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063efc81a8c14602a575b5f5ffd5b60306032565b005b5f604051603d90605a565b604051809103905ff0801580156055573d5f5f3e3d5ffd5b505050565b61013e806100688339019056fe6080604052348015600e575f5ffd5b506101228061001c5f395ff3fe608060405234801561000f575f5ffd5b506004361061003f575f3560e01c8063703c2d1a14610043578063affed0e01461004d578063b8dda9c714610069575b5f5ffd5b61004b61009b565b005b61005660015481565b6040519081526020015b60405180910390f35b61008b6100773660046100e6565b5f6020819052908152604090205460ff1681565b6040519015158152602001610060565b5f5b60648110156100e3576001805f8282546100b791906100fd565b9091555050600180545f908152602081905260409020805460ff19811660ff909116151790550161009d565b50565b5f602082840312156100f6575f5ffd5b5035919050565b8082018082111561011c57634e487b7160e01b5f52601160045260245ffd5b9291505056")]
//         contract SomeContractFactory {
//             constructor() {}

//             function create() external {
//                 address newContract = address(new SomeContract());
//             }
//         }

//         #[sol(bytecode = "608060405234801561000f575f5ffd5b506004361061003f575f3560e01c8063703c2d1a14610043578063affed0e01461004d578063b8dda9c714610069575b5f5ffd5b61004b61009b565b005b61005660015481565b6040519081526020015b60405180910390f35b61008b6100773660046100e6565b5f6020819052908152604090205460ff1681565b6040519015158152602001610060565b5f5b60648110156100e3576001805f8282546100b791906100fd565b9091555050600180545f908152602081905260409020805460ff19811660ff909116151790550161009d565b50565b5f602082840312156100f6575f5ffd5b5035919050565b8082018082111561011c57634e487b7160e01b5f52601160045260245ffd5b9291505056")]
//         contract SomeContract {
//             mapping(uint256 => bool) public map;
//             uint256 public nonce;

//             constructor() {}

//             function sstore() external {
//                 for (uint256 i = 0; i < 100; i++) {
//                     nonce += 1;
//                     bool value = map[nonce];
//                     map[nonce] = !value;
//                 }
//             }
//         }
//     }

//     lazy_static! {
//         static ref ALICE: PrivateKeySigner =
//             PrivateKeySigner::from_bytes(&[1u8; 32].into()).unwrap();
//         static ref BOB: PrivateKeySigner = PrivateKeySigner::from_bytes(&[2u8; 32].into()).unwrap();
//         static ref FACTORY: Address = address!("0x1234567890123456789012345678901234567890");
//         static ref GENESIS: Genesis = Genesis::default()
//             .extend_accounts(vec![
//                 (
//                     ALICE.address(),
//                     GenesisAccount::default()
//                         .with_balance(U256::from(10_u128.pow(21)))
//                         .with_nonce(Some(0))
//                 ),
//                 (
//                     BOB.address(),
//                     GenesisAccount::default()
//                         .with_balance(U256::ONE)
//                         .with_nonce(Some(0))
//                 ),
//             ])
//             .with_base_fee(Some(1))
//             .with_gas_limit(u64::MAX);
//         static ref CHAIN_SPEC: OpChainSpec = OpChainSpecBuilder::default()
//             .chain(GENESIS.config.chain_id.into())
//             .genesis(GENESIS.clone())
//             .ecotone_activated()
//             .build();
//         static ref EVM_CONFIG: OpEvmConfig =
//             OpEvmConfig::new(CHAIN_SPEC.clone().into(), OpRethReceiptBuilder::default());
//     }

//     #[derive(Debug)]
//     pub struct AccessListTest {
//         pub txs: Vec<Recovered<OpTransactionSigned>>,
//         pub expected: FlashblockAccessList,
//         pub db: State<InMemoryDB>,
//     }

//     impl AccessListTest {
//         pub fn new() -> Self {
//             let mut db = InMemoryDB::default();
//             for account in GENESIS.alloc.clone() {
//                 db.insert_account_info(
//                     account.0,
//                     AccountInfo {
//                         balance: account.1.balance,
//                         nonce: account.1.nonce.unwrap_or_default(),
//                         code_hash: account
//                             .1
//                             .code
//                             .as_ref()
//                             .map(keccak256)
//                             .unwrap_or(KECCAK_EMPTY),
//                         code: account.1.code.map(Bytecode::new_legacy),
//                     },
//                 )
//             }
//             Self {
//                 db: State::builder()
//                     .with_database(db)
//                     .with_bundle_update()
//                     .build(),
//                 txs: Vec::new(),
//                 expected: FlashblockAccessList::default(),
//             }
//         }

//         pub fn with_tx(
//             mut self,
//             from: PrivateKeySigner,
//             to: Option<Address>,
//             value: U256,
//             input: Bytes,
//         ) -> Self {
//             let mut tx = TxEip1559 {
//                 chain_id: CHAIN_SPEC.chain().id(),
//                 nonce: self
//                     .db
//                     .cache
//                     .accounts
//                     .get(&from.address())
//                     .map(|info| info.account.as_ref().map(|a| a.info.nonce).unwrap_or(0))
//                     .unwrap_or(0),
//                 max_fee_per_gas: CHAIN_SPEC.genesis_header().base_fee_per_gas.unwrap_or(1) as u128,
//                 to: to.map(TxKind::Call).unwrap_or(TxKind::Create),
//                 gas_limit: 500_000,
//                 value,
//                 input,
//                 ..Default::default()
//             };

//             let signed = from.sign_transaction_sync(&mut tx).unwrap();

//             let typed = OpTypedTransaction::Eip1559(tx);
//             let recovered = OpTxEnvelope::from((typed, signed))
//                 .try_into_recovered()
//                 .unwrap();

//             self.txs.push(recovered);
//             self
//         }

//         pub fn with_contract(mut self, contract: Address, code: Bytes) -> Self {
//             let code = Bytecode::new_legacy(code.clone());
//             self.db.insert_account(
//                 contract,
//                 AccountInfo {
//                     balance: U256::ZERO,
//                     nonce: 0,
//                     code_hash: keccak256(code.bytes()),
//                     code: Some(code),
//                 },
//             );
//             self
//         }

//         pub fn with_expected(mut self, expected: FlashblockAccessList) -> Self {
//             self.expected = expected;
//             self
//         }

//         pub fn test(
//             mut self,
//         ) -> (
//             BundleState,
//             BlockExecutionResult<OpReceipt>,
//             FlashblockAccessList,
//             u64,
//             u64,
//         ) {
//             let evm_env = EVM_CONFIG.evm_env(CHAIN_SPEC.genesis_header()).unwrap();
//             let evm = OpEvmFactory::default().create_evm(&mut self.db, evm_env);
//             let ctx = OpBlockExecutionCtx {
//                 parent_beacon_block_root: Some(FixedBytes::ZERO),
//                 parent_hash: CHAIN_SPEC.genesis_hash(),
//                 ..Default::default()
//             };

//             let mut executor = AsyncBalBuilderBlockExecutor::new(
//                 evm,
//                 ctx,
//                 CHAIN_SPEC.clone().into(),
//                 OpRethReceiptBuilder::default(),
//             );

//             let _ = executor.apply_pre_execution_changes();

//             for tx in self.txs {
//                 executor.execute_transaction(tx).unwrap();
//             }

//             let finish_result = executor.finish_with_access_list().unwrap();
//             let access_list = finish_result.access_list_data.access_list;

//             let (db, _) = finish_result.evm.finish();

//             assert_eq!(
//                 access_list, self.expected,
//                 "Access list does not match got {:#?}, expected {:#?}",
//                 access_list, self.expected
//             );

//             // merge all transitions into bundle state
//             db.merge_transitions(BundleRetention::Reverts);

//             // flatten reverts into a single reverts as the bundle is re-used across multiple payloads
//             // which represent a single atomic state transition. therefore reverts should have length 1
//             // we only retain the first occurance of a revert for any given account.
//             let flattened = db
//                 .bundle_state
//                 .reverts
//                 .iter()
//                 .flatten()
//                 .scan(HashSet::new(), |visited, (acc, revert)| {
//                     if visited.insert(acc) {
//                         Some((*acc, revert.clone()))
//                     } else {
//                         None
//                     }
//                 })
//                 .collect();

//             db.bundle_state.reverts = Reverts::new(vec![flattened]);

//             (
//                 db.bundle_state.clone(),
//                 finish_result.execution_result,
//                 access_list,
//                 finish_result.min_tx_index,
//                 finish_result.max_tx_index,
//             )
//         }
//     }

//     #[test]
//     #[ignore = "incorrect assertions"]
//     fn test_bal_balance_changes() {
//         AccessListTest::new()
//             .with_tx(
//                 ALICE.clone(),
//                 Some(BOB.address()),
//                 U256::from(100),
//                 Bytes::default(),
//             )
//             .with_expected(FlashblockAccessList {
//                 changes: vec![
//                     AccountChanges {
//                         address: address!("0x1a642f0e3c3af545e7acbd38b07251b3990914f1"),
//                         balance_changes: vec![BalanceChange {
//                             block_access_index: 1,
//                             post_balance: U256::from_str_radix("999999999999999978900", 10)
//                                 .unwrap(),
//                         }],
//                         nonce_changes: vec![NonceChange {
//                             block_access_index: 1,
//                             new_nonce: 1,
//                         }],
//                         ..Default::default()
//                     },
//                     AccountChanges {
//                         address: address!("0x4200000000000000000000000000000000000019"),

//                         balance_changes: vec![BalanceChange {
//                             block_access_index: 1,
//                             post_balance: U256::from(21000),
//                         }],
//                         ..Default::default()
//                     },
//                     AccountChanges {
//                         address: address!("0x5050a4f4b3f9338c3472dcc01a87c76a144b3c9c"),
//                         balance_changes: vec![BalanceChange {
//                             block_access_index: 1,
//                             post_balance: U256::from(101),
//                         }],
//                         ..Default::default()
//                     },
//                 ],
//                 min_tx_index: 0,
//                 max_tx_index: 1,
//             })
//             .test();
//     }

//     #[test]
//     #[ignore = "incorrect assertions"]
//     fn test_bal_code_changes() {
//         AccessListTest::new()
//             .with_contract(*FACTORY, SomeContractFactory::BYTECODE.clone())
//             .with_tx(
//                 ALICE.clone(),
//                 Some(*FACTORY),
//                 U256::ZERO,
//                 SomeContractFactory::createCall::SELECTOR.into(),
//             )
//             .with_expected(FlashblockAccessList {
//     changes: vec![
//         AccountChanges {
//             address: address!("0x1234567890123456789012345678901234567890"),
//             nonce_changes: vec![
//                 NonceChange {
//                     block_access_index: 1,
//                     new_nonce: 1,
//                 },
//             ],
//             code_changes: vec![
//                 CodeChange {
//                     block_access_index: 1,
//                     new_code: bytes!("6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063efc81a8c14602a575b5f5ffd5b60306032565b005b5f604051603d90605a565b604051809103905ff0801580156055573d5f5f3e3d5ffd5b505050565b61013e806100688339019056fe6080604052348015600e575f5ffd5b506101228061001c5f395ff3fe608060405234801561000f575f5ffd5b506004361061003f575f3560e01c8063703c2d1a14610043578063affed0e01461004d578063b8dda9c714610069575b5f5ffd5b61004b61009b565b005b61005660015481565b6040519081526020015b60405180910390f35b61008b6100773660046100e6565b5f6020819052908152604090205460ff1681565b6040519015158152602001610060565b5f5b60648110156100e3576001805f8282546100b791906100fd565b9091555050600180545f908152602081905260409020805460ff19811660ff909116151790550161009d565b50565b5f602082840312156100f6575f5ffd5b5035919050565b8082018082111561011c57634e487b7160e01b5f52601160045260245ffd5b9291505056"),
//                 },
//             ],
//             ..Default::default()
//         },
//         AccountChanges {
//             address: address!("0x1a642f0e3c3af545e7acbd38b07251b3990914f1"),
//             balance_changes: vec![
//                 BalanceChange {
//                     block_access_index: 1,
//                     post_balance: U256::from_str_radix("999999999999999888515", 10).unwrap(),
//                 },
//             ],
//             nonce_changes: vec![
//                 NonceChange {
//                     block_access_index: 1,
//                     new_nonce: 1,
//                 },
//             ],
//             ..Default::default()
//         },
//         AccountChanges {
//             address: address!("0x4200000000000000000000000000000000000019"),
//             balance_changes: vec![
//                 BalanceChange {
//                     block_access_index: 1,
//                     post_balance: U256::from(111485),
//                 },
//             ],
//           ..Default::default()
//         },
//         AccountChanges {
//             address: address!("0xf831de50f3884cf0f8550bb129032a80cb5a26b7"),
//             nonce_changes: vec![
//                 NonceChange {
//                     block_access_index: 1,
//                     new_nonce: 1,
//                 },
//             ],
//             code_changes: vec![
//                 CodeChange {
//                     block_access_index: 1,
//                     new_code: bytes!("0x608060405234801561000f575f5ffd5b506004361061003f575f3560e01c8063703c2d1a14610043578063affed0e01461004d578063b8dda9c714610069575b5f5ffd5b61004b61009b565b005b61005660015481565b6040519081526020015b60405180910390f35b61008b6100773660046100e6565b5f6020819052908152604090205460ff1681565b6040519015158152602001610060565b5f5b60648110156100e3576001805f8282546100b791906100fd565b9091555050600180545f908152602081905260409020805460ff19811660ff909116151790550161009d565b50565b5f602082840312156100f6575f5ffd5b5035919050565b8082018082111561011c57634e487b7160e01b5f52601160045260245ffd5b9291505056"),
//                 },
//             ],
//             ..Default::default()
//         },
//     ],
//     min_tx_index: 0,
//     max_tx_index: 1,
// })
//             .test();
//     }

//     #[test]
//     #[ignore = "incorrect assertions"]
//     fn test_bal_state_root_computation() {
//         let (expected_bundle, _, access_list, _, _) = AccessListTest::new()
//             .with_tx(
//                 ALICE.clone(),
//                 Some(BOB.address()),
//                 U256::ONE,
//                 Bytes::default(),
//             )
//             .with_expected(FlashblockAccessList {
//                 changes: vec![
//                     AccountChanges {
//                         address: address!("0x1a642f0e3c3af545e7acbd38b07251b3990914f1"),
//                         balance_changes: vec![BalanceChange {
//                             block_access_index: 2,
//                             post_balance: U256::from_str_radix("999999999999999978999", 10)
//                                 .unwrap(),
//                         }],
//                         nonce_changes: vec![NonceChange {
//                             block_access_index: 1,
//                             new_nonce: 1,
//                         }],
//                         ..Default::default()
//                     },
//                     AccountChanges {
//                         address: address!("0x4200000000000000000000000000000000000019"),
//                         balance_changes: vec![BalanceChange {
//                             block_access_index: 1,
//                             post_balance: U256::from(111485),
//                         }],
//                         ..Default::default()
//                     },
//                     AccountChanges {
//                         address: address!("0x5050a4f4b3f9338c3472dcc01a87c76a144b3c9c"),
//                         balance_changes: vec![BalanceChange {
//                             block_access_index: 1,
//                             post_balance: U256::from(2),
//                         }],
//                         ..Default::default()
//                     },
//                 ],
//                 min_tx_index: 0,
//                 max_tx_index: 1,
//             })
//             .test();

//         let bundle: alloy_primitives::map::HashMap<Address, BundleAccount> = access_list.into();

//         for (address, account) in bundle.iter() {
//             let expected = expected_bundle.state.get(address).unwrap();
//             assert_eq!(
//                 account.info, expected.info,
//                 "Account Info mismatch for account {address:?}"
//             );
//             assert_eq!(
//                 account.storage, expected.storage,
//                 "Storage mismatch for account {address:?}"
//             );
//         }
//     }
// }
