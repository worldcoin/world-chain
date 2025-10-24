use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, U256};
use dashmap::DashMap;
use flashblocks_primitives::access_list::FlashblockAccessList;
use rayon::prelude::*;
use revm::{database::TransitionState, state::Bytecode};
use std::collections::{HashMap, HashSet};

pub type BlockAccessIndex = u16;

/// A convenience builder type for [`FlashblockAccessList`]
#[derive(Default, Debug, Clone)]
pub struct FlashblockAccessListConstruction {
    /// Map from Address -> AccountChangesConstruction
    pub changes: DashMap<Address, AccountChangesConstruction>,
}

impl FlashblockAccessListConstruction {
    /// Creates a new empty [`FlashblockAccessListConstruction`]
    pub fn new() -> Self {
        Self {
            changes: DashMap::new(),
        }
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
                    new_code: bytecode.bytes(),
                })
                .collect(),
        }
    }
}

impl FlashblockAccessListConstruction {
    /// Consumes the builder and produces a [`FlashblockAccessList`]
    pub fn build(self) -> FlashblockAccessList {
        // Sort addresses lexicographically
        let mut changes: Vec<_> = self
            .changes
            .into_par_iter()
            .map(|(k, v)| v.build(k))
            .collect();

        changes.par_sort_unstable_by_key(|a| a.address);

        FlashblockAccessList { changes }
    }
}

impl FlashblockAccessListConstruction {
    pub fn on_state_transition(&self, transitions: Option<&TransitionState>, index: usize) {
        let transitions = match transitions {
            Some(t) => t,
            None => return,
        };

        let index = index as BlockAccessIndex;
        transitions
            .transitions
            .par_iter()
            .for_each(|(address, transition)| {
                if !transition.status.is_not_modified() {
                    let mut acc_changes = self.changes.entry(*address).or_default();

                    // Storage changes
                    for (slot, slot_value) in &transition.storage {
                        if slot_value.is_changed() {
                            acc_changes
                                .storage_changes
                                .entry(*slot)
                                .or_default()
                                .insert(index, slot_value.present_value());
                        } else {
                            acc_changes.storage_reads.insert(*slot);
                        }
                    }

                    if let Some(present_info) = &transition.info {
                        if transition
                            .previous_info
                            .as_ref()
                            .map(|info| info.nonce != present_info.nonce)
                            .unwrap_or(true)
                        {
                            acc_changes
                                .nonce_changes
                                .entry(index)
                                .and_modify(|nonce| *nonce = present_info.nonce)
                                .or_insert(present_info.nonce);
                        }

                        if transition
                            .previous_info
                            .as_ref()
                            .map(|info| info.balance != present_info.balance)
                            .unwrap_or(true)
                        {
                            acc_changes
                                .balance_changes
                                .entry(index)
                                .and_modify(|bal| *bal = present_info.balance)
                                .or_insert(present_info.balance);
                        }

                        if transition
                            .previous_info
                            .as_ref()
                            .map(|info| info.code_hash != present_info.code_hash)
                            .unwrap_or(true)
                        {
                            if let Some(code) = &present_info.code {
                                acc_changes
                                    .code_changes
                                    .entry(index)
                                    .and_modify(|bc| *bc = code.clone())
                                    .or_insert_with(|| code.to_owned());
                            }
                        }
                    }
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::executor::FlashblocksBlockExecutor;
    use alloy_consensus::{constants::KECCAK_EMPTY, TxEip1559};
    use alloy_eip7928::{AccountChanges, BalanceChange, CodeChange, NonceChange};
    use alloy_genesis::{Genesis, GenesisAccount};
    use alloy_op_evm::{OpBlockExecutionCtx, OpEvmFactory};
    use alloy_primitives::{address, bytes, keccak256, Address, Bytes, FixedBytes, TxKind, U256};

    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::{sol, SolCall};
    use flashblocks_primitives::access_list::FlashblockAccessList;
    use lazy_static::lazy_static;
    use op_alloy_consensus::{OpTxEnvelope, OpTypedTransaction};
    use op_alloy_network::TxSignerSync;
    use reth::revm::State;
    use reth_evm::{block::BlockExecutor, ConfigureEvm, EvmFactory};
    use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
    use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
    use reth_optimism_primitives::OpTransactionSigned;
    use reth_primitives::{transaction::SignedTransaction, Recovered};
    use revm::{
        database::InMemoryDB,
        state::{AccountInfo, Bytecode},
    };

    sol! {
        #[sol(bytecode = "0x6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063efc81a8c14602a575b5f5ffd5b60306032565b005b5f604051603d90605a565b604051809103905ff0801580156055573d5f5f3e3d5ffd5b505050565b61013e806100688339019056fe6080604052348015600e575f5ffd5b506101228061001c5f395ff3fe608060405234801561000f575f5ffd5b506004361061003f575f3560e01c8063703c2d1a14610043578063affed0e01461004d578063b8dda9c714610069575b5f5ffd5b61004b61009b565b005b61005660015481565b6040519081526020015b60405180910390f35b61008b6100773660046100e6565b5f6020819052908152604090205460ff1681565b6040519015158152602001610060565b5f5b60648110156100e3576001805f8282546100b791906100fd565b9091555050600180545f908152602081905260409020805460ff19811660ff909116151790550161009d565b50565b5f602082840312156100f6575f5ffd5b5035919050565b8082018082111561011c57634e487b7160e01b5f52601160045260245ffd5b9291505056")]
        contract SomeContractFactory {
            constructor() {}

            function create() external {
                address newContract = address(new SomeContract());
            }
        }

        #[sol(bytecode = "0x608060405234801561000f575f5ffd5b506004361061003f575f3560e01c8063703c2d1a14610043578063affed0e01461004d578063b8dda9c714610069575b5f5ffd5b61004b61009b565b005b61005660015481565b6040519081526020015b60405180910390f35b61008b6100773660046100e6565b5f6020819052908152604090205460ff1681565b6040519015158152602001610060565b5f5b60648110156100e3576001805f8282546100b791906100fd565b9091555050600180545f908152602081905260409020805460ff19811660ff909116151790550161009d565b50565b5f602082840312156100f6575f5ffd5b5035919050565b8082018082111561011c57634e487b7160e01b5f52601160045260245ffd5b929150505600")]
        contract SomeContract {
            mapping(uint256 => bool) public map;
            uint256 public nonce;

            constructor() {}

            function sstore() external {
                for (uint256 i = 0; i < 100; i++) {
                    nonce += 1;
                    bool value = map[nonce];
                    map[nonce] = !value;
                }
            }
        }
    }

    lazy_static! {
        static ref ALICE: PrivateKeySigner =
            PrivateKeySigner::from_bytes(&[1u8; 32].into()).unwrap();
        static ref BOB: PrivateKeySigner = PrivateKeySigner::from_bytes(&[2u8; 32].into()).unwrap();
        static ref FACTORY: Address = address!("0x1234567890123456789012345678901234567890");
        static ref GENESIS: Genesis = Genesis::default()
            .extend_accounts(vec![
                (
                    ALICE.address(),
                    GenesisAccount::default()
                        .with_balance(U256::from(10_u128.pow(21)))
                        .with_nonce(Some(0))
                ),
                (
                    BOB.address(),
                    GenesisAccount::default()
                        .with_balance(U256::ONE)
                        .with_nonce(Some(0))
                ),
            ])
            .with_base_fee(Some(1))
            .with_gas_limit(u64::MAX);
        static ref CHAIN_SPEC: OpChainSpec = OpChainSpecBuilder::default()
            .chain(GENESIS.config.chain_id.into())
            .genesis(GENESIS.clone())
            .ecotone_activated()
            .build();
        static ref EVM_CONFIG: OpEvmConfig =
            OpEvmConfig::new(CHAIN_SPEC.clone().into(), OpRethReceiptBuilder::default());
    }

    #[derive(Debug)]
    pub struct AccessListTest {
        pub txs: Vec<Recovered<OpTransactionSigned>>,
        pub expected: FlashblockAccessList,
        pub db: State<InMemoryDB>,
    }

    impl AccessListTest {
        pub fn new() -> Self {
            let mut db = InMemoryDB::default();
            for account in GENESIS.alloc.clone() {
                db.insert_account_info(
                    account.0,
                    AccountInfo {
                        balance: account.1.balance,
                        nonce: account.1.nonce.unwrap_or_default(),
                        code_hash: account
                            .1
                            .code
                            .as_ref()
                            .map(keccak256)
                            .unwrap_or(KECCAK_EMPTY),
                        code: account.1.code.map(Bytecode::new_legacy),
                    },
                )
            }
            Self {
                db: State::builder()
                    .with_database(db)
                    .with_bundle_update()
                    .build(),
                txs: Vec::new(),
                expected: FlashblockAccessList::default(),
            }
        }

        pub fn with_tx(
            mut self,
            from: PrivateKeySigner,
            to: Option<Address>,
            value: U256,
            input: Bytes,
        ) -> Self {
            let mut tx = TxEip1559 {
                chain_id: CHAIN_SPEC.chain().id(),
                nonce: self
                    .db
                    .cache
                    .accounts
                    .get(&from.address())
                    .map(|info| info.account.as_ref().map(|a| a.info.nonce).unwrap_or(0))
                    .unwrap_or(0),
                max_fee_per_gas: CHAIN_SPEC.genesis_header().base_fee_per_gas.unwrap_or(1) as u128,
                to: to.map(TxKind::Call).unwrap_or(TxKind::Create),
                gas_limit: 500_000,
                value,
                input,
                ..Default::default()
            };

            let signed = from.sign_transaction_sync(&mut tx).unwrap();

            let typed = OpTypedTransaction::Eip1559(tx);
            let recovered = OpTxEnvelope::from((typed, signed))
                .try_into_recovered()
                .unwrap();

            self.txs.push(recovered);
            self
        }

        pub fn with_contract(mut self, contract: Address, code: Bytes) -> Self {
            let code = Bytecode::new_legacy(code.clone());
            self.db.insert_account(
                contract,
                AccountInfo {
                    balance: U256::ZERO,
                    nonce: 0,
                    code_hash: keccak256(code.bytes()),
                    code: Some(code),
                },
            );
            self
        }

        pub fn with_expected(mut self, expected: FlashblockAccessList) -> Self {
            self.expected = expected;
            self
        }

        pub fn test(mut self) {
            let evm_env = EVM_CONFIG.evm_env(CHAIN_SPEC.genesis_header()).unwrap();
            let evm = OpEvmFactory::default().create_evm(&mut self.db, evm_env);
            let ctx = OpBlockExecutionCtx {
                parent_beacon_block_root: Some(FixedBytes::ZERO),
                parent_hash: CHAIN_SPEC.genesis_hash(),
                ..Default::default()
            };

            let mut executor = FlashblocksBlockExecutor::new(
                evm,
                ctx,
                CHAIN_SPEC.clone(),
                OpRethReceiptBuilder::default(),
            );

            let _ = executor.apply_pre_execution_changes();

            for tx in self.txs {
                executor.execute_transaction(tx).unwrap();
            }

            let (_, _, access_list) = executor.finish_with_access_list().unwrap();
            let access_list = access_list.build();

            assert_eq!(
                access_list, self.expected,
                "Access list does not match got {:#?}, expected {:#?}",
                access_list, self.expected
            );
        }
    }

    #[test]
    fn test_bal_balance_changes() {
        let expected = FlashblockAccessList {
            changes: vec![
                AccountChanges {
                    address: ALICE.address(),
                    balance_changes: vec![BalanceChange {
                        block_access_index: 1,
                        post_balance: U256::from(10_u128.pow(21) - 100 - 21000),
                    }],
                    nonce_changes: vec![NonceChange {
                        block_access_index: 1,
                        new_nonce: 1,
                    }],
                    ..Default::default()
                },
                AccountChanges {
                    address: address!("0x4200000000000000000000000000000000000019"),
                    balance_changes: vec![BalanceChange {
                        block_access_index: 1,
                        post_balance: U256::from(21000),
                    }],
                    code_changes: vec![CodeChange {
                        block_access_index: 1,
                        new_code: bytes!("0x00"),
                    }],
                    nonce_changes: vec![NonceChange {
                        block_access_index: 1,
                        new_nonce: 0,
                    }],
                    ..Default::default()
                },
                AccountChanges {
                    address: BOB.address(),
                    balance_changes: vec![BalanceChange {
                        block_access_index: 1,
                        post_balance: U256::from(101),
                    }],
                    ..Default::default()
                },
            ],
        };

        AccessListTest::new()
            .with_tx(
                ALICE.clone(),
                Some(BOB.address()),
                U256::from(100),
                Bytes::default(),
            )
            .with_expected(expected)
            .test();
    }

    #[test]
    fn test_bal_code_changes() {
        AccessListTest::new()
            .with_contract(*FACTORY, SomeContractFactory::BYTECODE.clone())
            .with_tx(
                ALICE.clone(),
                Some(*FACTORY),
                U256::ZERO,
                SomeContractFactory::createCall::SELECTOR.into(),
            )
            .with_expected(FlashblockAccessList {
                changes: vec![
                    AccountChanges {
                        address: *FACTORY,
                        nonce_changes: vec![NonceChange {
                            block_access_index: 1,
                            new_nonce: 1,
                        }],
                        ..Default::default()
                    },
                    AccountChanges {
                        address: ALICE.address(),
                        balance_changes: vec![BalanceChange {
                            block_access_index: 1,
                            post_balance: U256::from_str_radix("999999999999999888515", 10)
                                .unwrap(),
                        }],
                        nonce_changes: vec![NonceChange {
                            block_access_index: 1,
                            new_nonce: 1,
                        }],
                        ..Default::default()
                    },
                    AccountChanges {
                        address: address!("0x4200000000000000000000000000000000000019"),
                        balance_changes: vec![BalanceChange {
                            block_access_index: 1,
                            post_balance: U256::from(111485),
                        }],
                        nonce_changes: vec![NonceChange {
                            block_access_index: 1,
                            new_nonce: 0,
                        }],
                        code_changes: vec![CodeChange {
                            block_access_index: 1,
                            new_code: bytes!("0x00"),
                        }],
                        ..Default::default()
                    },
                    AccountChanges {
                        address: address!("0xf831de50f3884cf0f8550bb129032a80cb5a26b7"),
                        balance_changes: vec![BalanceChange {
                            block_access_index: 1,
                            post_balance: U256::ZERO,
                        }],
                        nonce_changes: vec![NonceChange {
                            block_access_index: 1,
                            new_nonce: 1,
                        }],
                        code_changes: vec![CodeChange {
                            block_access_index: 1,
                            new_code: SomeContract::BYTECODE.clone(),
                        }],
                        ..Default::default()
                    },
                ],
            })
            .test();
    }
}
