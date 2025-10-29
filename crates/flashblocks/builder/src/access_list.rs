use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{address, bytes, Address, Bytes, U256};
use dashmap::DashMap;
use flashblocks_primitives::access_list::FlashblockAccessList;
use lazy_static::lazy_static;
use rayon::prelude::*;
use revm::{database::TransitionState, state::Bytecode};
use std::collections::{HashMap, HashSet};
use tracing::{info, trace};

pub type BlockAccessIndex = u16;

const L1_BLOCK_INFO_ADDRESS: Address = address!("4200000000000000000000000000000000000015");
const L1_BLOCK_INFO_BYTECODE: Bytes = bytes!("60806040526004361061005e5760003560e01c80635c60da1b116100435780635c60da1b146100be5780638f283970146100f8578063f851a440146101185761006d565b80633659cfe6146100755780634f1ef286146100955761006d565b3661006d5761006b61012d565b005b61006b61012d565b34801561008157600080fd5b5061006b6100903660046106dd565b610224565b6100a86100a33660046106f8565b610296565b6040516100b5919061077b565b60405180910390f35b3480156100ca57600080fd5b506100d3610419565b60405173ffffffffffffffffffffffffffffffffffffffff90911681526020016100b5565b34801561010457600080fd5b5061006b6101133660046106dd565b6104b0565b34801561012457600080fd5b506100d3610517565b60006101577f360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc5490565b905073ffffffffffffffffffffffffffffffffffffffff8116610201576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152602560248201527f50726f78793a20696d706c656d656e746174696f6e206e6f7420696e6974696160448201527f6c697a656400000000000000000000000000000000000000000000000000000060648201526084015b60405180910390fd5b3660008037600080366000845af43d6000803e8061021e573d6000fd5b503d6000f35b7fb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d61035473ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff16148061027d575033155b1561028e5761028b816105a3565b50565b61028b61012d565b60606102c07fb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d61035490565b73ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff1614806102f7575033155b1561040a57610305846105a3565b6000808573ffffffffffffffffffffffffffffffffffffffff16858560405161032f9291906107ee565b600060405180830381855af49150503d806000811461036a576040519150601f19603f3d011682016040523d82523d6000602084013e61036f565b606091505b509150915081610401576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152603960248201527f50726f78793a2064656c656761746563616c6c20746f206e657720696d706c6560448201527f6d656e746174696f6e20636f6e7472616374206661696c65640000000000000060648201526084016101f8565b91506104129050565b61041261012d565b9392505050565b60006104437fb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d61035490565b73ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff16148061047a575033155b156104a557507f360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc5490565b6104ad61012d565b90565b7fb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d61035473ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff161480610509575033155b1561028e5761028b8161060c565b60006105417fb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d61035490565b73ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff161480610578575033155b156104a557507fb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d61035490565b7f360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc81815560405173ffffffffffffffffffffffffffffffffffffffff8316907fbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b90600090a25050565b60006106367fb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d61035490565b7fb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d61038381556040805173ffffffffffffffffffffffffffffffffffffffff80851682528616602082015292935090917f7e644d79422f17c01e4894b5f4f588d331ebfa28653d42ae832dc59e38c9798f910160405180910390a1505050565b803573ffffffffffffffffffffffffffffffffffffffff811681146106d857600080fd5b919050565b6000602082840312156106ef57600080fd5b610412826106b4565b60008060006040848603121561070d57600080fd5b610716846106b4565b9250602084013567ffffffffffffffff8082111561073357600080fd5b818601915086601f83011261074757600080fd5b81358181111561075657600080fd5b87602082850101111561076857600080fd5b6020830194508093505050509250925092565b600060208083528351808285015260005b818110156107a85785810183015185820160400152820161078c565b818111156107ba576000604083870101525b50601f017fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe016929092016040019392505050565b818382376000910190815291905056fea164736f6c634300080f000a");

const HISTORY_STORAGE_ADDRESS: Address = address!("0000f90827f1c53a10cb7a02335b175320002935");
const HISTORY_STORAGE_BYTECODE: Bytes = bytes!("3373fffffffffffffffffffffffffffffffffffffffe14604657602036036042575f35600143038111604257611fff81430311604257611fff9006545f5260205ff35b5f5ffd5b5f35611fff60014303065500");

const BEACON_BLOCK_ROOT_ADDRESS: Address = address!("000f3df6d732807ef1319fb7b8bb8522d0beac02");
const BEACON_BLOCK_ROOT_BYTECODE: Bytes = bytes!("3373fffffffffffffffffffffffffffffffffffffffe14604d57602036146024575f5ffd5b5f35801560495762001fff810690815414603c575f5ffd5b62001fff01545f5260205ff35b5f5ffd5b62001fff42064281555f359062001fff015500");

lazy_static! {
    static ref PREDEPLOYS_MAP: HashMap<Address, Bytecode> = HashMap::from_iter(vec![
        (
            L1_BLOCK_INFO_ADDRESS,
            Bytecode::new_raw(L1_BLOCK_INFO_BYTECODE)
        ),
        (
            HISTORY_STORAGE_ADDRESS,
            Bytecode::new_raw(HISTORY_STORAGE_BYTECODE)
        ),
        (
            BEACON_BLOCK_ROOT_ADDRESS,
            Bytecode::new_raw(BEACON_BLOCK_ROOT_BYTECODE)
        )
    ]);
}

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
                    new_code: bytecode.original_bytes(),
                })
                .collect(),
        }
    }
}

impl FlashblockAccessListConstruction {
    /// Consumes the builder and produces a [`FlashblockAccessList`]
    pub fn build(self, min_tx_index: u64, max_tx_index: u64) -> FlashblockAccessList {
        trace!(target: "test_target", "Building FlashblockAccessList with {} account changes", self.changes.len());
        // Sort addresses lexicographically
        let mut changes: Vec<_> = self
            .changes
            .into_par_iter()
            .map(|(k, v)| v.build(k))
            .collect();

        trace!(target: "test_target", "Built {} account changes", changes.len());
        changes.par_sort_unstable_by_key(|a| a.address);

        FlashblockAccessList {
            changes,
            min_tx_index,
            max_tx_index,
        }
    }

    pub fn with_transition_state(&self, transitions: Option<&TransitionState>, index: usize) {
        info!(target: "test_target", "Processing state transition for tx index {} changes length {}", index, self.changes.len());
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

                    if PREDEPLOYS_MAP.contains_key(address) {
                        acc_changes
                            .code_changes
                            .insert(index, PREDEPLOYS_MAP.get(address).unwrap().clone());
                        
                        // Set the None, Bytecode, and Code Hash to the previous info
                        if let Some(prev_info) = transition.previous_info.as_ref() {
                            acc_changes
                                .nonce_changes
                                .entry(index)
                                .and_modify(|nonce| *nonce = prev_info.nonce)
                                .or_insert(prev_info.nonce);
                        }
                    }

                    if let Some(present_info) = &transition.info {
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

                        if PREDEPLOYS_MAP.contains_key(address) {
                            return;
                        }

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
    use crate::executor::bal_builder::BalBuilderBlockExecutor;
    use alloy_consensus::{constants::KECCAK_EMPTY, TxEip1559};
    use alloy_eip7928::{AccountChanges, BalanceChange, CodeChange, NonceChange};
    use alloy_genesis::{Genesis, GenesisAccount};
    use alloy_op_evm::{OpBlockExecutionCtx, OpEvmFactory};
    use alloy_primitives::{address, keccak256, Address, Bytes, FixedBytes, TxKind, U256};

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
    use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
    use reth_primitives::{transaction::SignedTransaction, Recovered};
    use reth_provider::BlockExecutionResult;
    use revm::{
        context::ContextTr,
        database::{BundleState, InMemoryDB},
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

        #[sol(bytecode = "608060405234801561000f575f5ffd5b506004361061003f575f3560e01c8063703c2d1a14610043578063affed0e01461004d578063b8dda9c714610069575b5f5ffd5b61004b61009b565b005b61005660015481565b6040519081526020015b60405180910390f35b61008b6100773660046100e6565b5f6020819052908152604090205460ff1681565b6040519015158152602001610060565b5f5b60648110156100e3576001805f8282546100b791906100fd565b9091555050600180545f908152602081905260409020805460ff19811660ff909116151790550161009d565b50565b5f602082840312156100f6575f5ffd5b5035919050565b8082018082111561011c57634e487b7160e01b5f52601160045260245ffd5b9291505056")]
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

        pub fn test(
            mut self,
        ) -> (
            BundleState,
            BlockExecutionResult<OpReceipt>,
            FlashblockAccessList,
            u64,
            u64,
        ) {
            let evm_env = EVM_CONFIG.evm_env(CHAIN_SPEC.genesis_header()).unwrap();
            let evm = OpEvmFactory::default().create_evm(&mut self.db, evm_env);
            let ctx = OpBlockExecutionCtx {
                parent_beacon_block_root: Some(FixedBytes::ZERO),
                parent_hash: CHAIN_SPEC.genesis_hash(),
                ..Default::default()
            };

            let mut executor = BalBuilderBlockExecutor::new(
                evm,
                ctx,
                CHAIN_SPEC.clone(),
                OpRethReceiptBuilder::default(),
                0,
            );

            let _ = executor.apply_pre_execution_changes();

            for tx in self.txs {
                executor.execute_transaction(tx).unwrap();
            }

            let (mut evm, execution_result, access_list, min_tx_index, max_tx_index) =
                executor.finish_with_access_list().unwrap();

            let access_list = access_list.build(0, 1);

            assert_eq!(
                access_list, self.expected,
                "Access list does not match got {:#?}, expected {:#?}",
                access_list, self.expected
            );

            let bundle = evm.db_mut().take_bundle();

            (
                bundle,
                execution_result,
                access_list,
                min_tx_index,
                max_tx_index,
            )
        }
    }

    #[test]
    fn test_bal_balance_changes() {
        AccessListTest::new()
            .with_tx(
                ALICE.clone(),
                Some(BOB.address()),
                U256::from(100),
                Bytes::default(),
            )
            .with_expected(FlashblockAccessList {
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
                            new_code: Bytes::default(),
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
                max_tx_index: 1,
                min_tx_index: 0,
            })
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
                            new_code: Bytes::default(),
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
                max_tx_index: 1,
                min_tx_index: 0,
            })
            .test();
    }

    #[test]
    fn test_bal_state_root_computation() {
        // let (bundle_state, outcome, access_list, min_tx_index, max_tx_index) =
        //     AccessListTest::new()
        //         .with_tx(
        //             ALICE.clone(),
        //             Some(BOB.address()),
        //             U256::ONE,
        //             Bytes::default(),
        //         )
        //         .with_tx(
        //             ALICE.clone(),
        //             Some(BOB.address()),
        //             U256::ONE,
        //             Bytes::default(),
        //         )
        //         .with_tx(
        //             ALICE.clone(),
        //             Some(BOB.address()),
        //             U256::ONE,
        //             Bytes::default(),
        //         )
        //         .with_expected(Default::default())
        //         .test();

        // Re-construct the state root both from the created bundle from the executor, and from the converted access list
        // let constructed_bundl_hash = compute_state_root()
    }
}
