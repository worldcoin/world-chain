use alloy_consensus::{BlockHeader, TxEip1559, constants::KECCAK_EMPTY};
use alloy_eips::eip2718::Encodable2718;
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor, OpEvmFactory};
use alloy_primitives::{Address, B256, Bytes, FixedBytes, TxKind, U256, hex, keccak256};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, sol};
use alloy_trie::TrieAccount;
use crossbeam_channel::bounded;
use flashblocks_builder::{
    database::bal_builder_db::BalBuilderDb,
    executor::{BalBlockBuilder, CommittedState},
};
use flashblocks_primitives::{
    access_list::{FlashblockAccessListData, access_list_hash},
    primitives::ExecutionPayloadFlashblockDeltaV1,
};
use op_alloy_consensus::{OpTxEnvelope, OpTypedTransaction};
use op_alloy_network::TxSignerSync;
use proptest::prelude::*;
use reth::revm::{State, database::StateProviderDatabase};
use reth_evm::{
    ConfigureEvm, EvmEnv, EvmFactory,
    block::CommitChanges,
    execute::{BlockBuilder, BlockBuilderOutcome},
    op_revm::OpSpecId,
};
use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
use reth_optimism_evm::{OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder};
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives::{Account, Recovered, SealedHeader, transaction::SignedTransaction};
use reth_provider::{BytecodeReader, StateProvider};
use reth_trie_common::HashedPostState;
use revm::{
    DatabaseRef,
    database::{BundleState, InMemoryDB},
    state::AccountInfo,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

lazy_static::lazy_static! {
    /// Test signer with deterministic private key
    pub static ref ALICE: PrivateKeySigner =
        PrivateKeySigner::from_bytes(&[1u8; 32].into()).unwrap();

    /// Second test signer
    pub static ref BOB: PrivateKeySigner =
        PrivateKeySigner::from_bytes(&[2u8; 32].into()).unwrap();

    /// Third test signer
    pub static ref CHARLIE: PrivateKeySigner =
        PrivateKeySigner::from_bytes(&[3u8; 32].into()).unwrap();

    /// ChaosTest contract address (deterministic)
    pub static ref CHAOS_CONTRACT: Address =
        Address::from_slice(&[0xCA, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);

    /// ChaosTestProxy contract address (deterministic)
    pub static ref CHAOS_PROXY_CONTRACT: Address =
        Address::from_slice(&[0xCA, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02]);

    /// ChaosTest contract bytecode (from sol! macro compilation)
    pub static ref CHAOS_BYTECODE: Bytes = Bytes::from(
        hex::decode("6080604052348015600e575f5ffd5b5060b680601a5f395ff3fe6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063c6c2ea1714602a575b5f5ffd5b6039603536600460a0565b604b565b60405190815260200160405180910390f35b5f6001818155600255818015608e576001811460955760025b6001840181101560825780545f198201540160019091019081556064565b5082600101549150609a565b5f9150609a565b600191505b50919050565b5f6020828403121560af575f5ffd5b503591905056")
            .expect("valid hex")
    );

    /// ChaosTestProxy bytecode - delegates to ChaosTest
    pub static ref CHAOS_PROXY_BYTECODE: Bytes = Bytes::from(
        hex::decode("608060405260043610601e575f3560e01c8063b243cd76146067576024565b36602457005b7f360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc54365f5f375f5f365f845af490503d5f5f3e8080156061573d5ff35b3d5ffd5b005b3480156071575f5ffd5b5060655f604051607f9060bd565b604051809103905ff0801580156097573d5f5f3e3d5ffd5b507f360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc5550565b60d0806100ca8339019056fe6080604052348015600e575f5ffd5b5060b680601a5f395ff3fe6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063c6c2ea1714602a575b5f5ffd5b6039603536600460a0565b604b565b60405190815260200160405180910390f35b5f6001818155600255818015608e576001811460955760025b6001840181101560825780545f198201540160019091019081556064565b5082600101549150609a565b5f9150609a565b600191505b50919050565b5f6020828403121560af575f5ffd5b503591905056")
            .expect("valid hex")
    );

    /// Proxy storage with implementation address set
    pub static ref CHAOS_PROXY_STORAGE: BTreeMap<B256, B256> = {
        let mut storage = BTreeMap::new();
        // IMPL_SLOT = 0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc
        let impl_slot = B256::from_slice(&hex::decode("360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc").unwrap());
        // Store ChaosTest address (left-padded to 32 bytes)
        let mut impl_value = [0u8; 32];
        impl_value[12..32].copy_from_slice(CHAOS_CONTRACT.as_slice());
        storage.insert(impl_slot, B256::from(impl_value));
        storage
    };

    /// Genesis configuration with funded accounts and ChaosTest contracts
    pub static ref GENESIS: Genesis = Genesis::default()
        .extend_accounts(vec![
            (
                ALICE.address(),
                GenesisAccount::default()
                    .with_balance(U256::from(10u128.pow(21))) // 1000 ETH
                    .with_nonce(Some(0))
            ),
            (
                BOB.address(),
                GenesisAccount::default()
                    .with_balance(U256::from(10u128.pow(21)))
                    .with_nonce(Some(0))
            ),
            (
                CHARLIE.address(),
                GenesisAccount::default()
                    .with_balance(U256::from(10u128.pow(21)))
                    .with_nonce(Some(0))
            ),
            (
                *CHAOS_CONTRACT,
                GenesisAccount::default()
                    .with_code(Some(CHAOS_BYTECODE.clone()))
                    .with_nonce(Some(1))
            ),
            (
                *CHAOS_PROXY_CONTRACT,
                GenesisAccount::default()
                    .with_code(Some(CHAOS_PROXY_BYTECODE.clone()))
                    .with_storage(Some(CHAOS_PROXY_STORAGE.clone()))
                    .with_nonce(Some(1))
            ),
        ])
        .with_base_fee(Some(1))
        .with_gas_limit(30_000_000);

    /// Chain spec for tests
    pub static ref CHAIN_SPEC: Arc<OpChainSpec> = Arc::new(
        OpChainSpecBuilder::default()
            .chain(GENESIS.config.chain_id.into())
            .genesis(GENESIS.clone())
            .isthmus_activated()
            .build()
    );

    /// EVM configuration for tests
    pub static ref EVM_CONFIG: OpEvmConfig =
        OpEvmConfig::new(CHAIN_SPEC.clone(), OpRethReceiptBuilder::default());

    pub static ref BLOCK_EXECUTION_CTX: OpBlockExecutionCtx = OpBlockExecutionCtx {
        parent_beacon_block_root: Some(FixedBytes::ZERO),
        parent_hash: CHAIN_SPEC.genesis_hash(),
        ..Default::default()
    };

    pub static ref NEXT_BLOCK_ENV_ATTRS: OpNextBlockEnvAttributes = OpNextBlockEnvAttributes {
        timestamp: CHAIN_SPEC.genesis_header().timestamp() + 1,
        suggested_fee_recipient: Address::ZERO,
        prev_randao: FixedBytes::ZERO,
        gas_limit: CHAIN_SPEC.genesis_header().gas_limit,
        parent_beacon_block_root: Some(FixedBytes::ZERO),
        extra_data: Bytes::default(),
    };

    pub static ref SEALED_HEADER: SealedHeader = SealedHeader::seal_slow(CHAIN_SPEC.genesis_header().clone());

    pub static ref EVM_ENV: EvmEnv<OpSpecId> = EVM_CONFIG
        .next_evm_env(SEALED_HEADER.header(), &NEXT_BLOCK_ENV_ATTRS)
        .unwrap();
}

sol! {
    #[sol(bytecode = "0x6080604052348015600e575f5ffd5b5060b680601a5f395ff3fe6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063c6c2ea1714602a575b5f5ffd5b6039603536600460a0565b604b565b60405190815260200160405180910390f35b5f6001818155600255818015608e576001811460955760025b6001840181101560825780545f198201540160019091019081556064565b5082600101549150609a565b5f9150609a565b600191505b50919050565b5f6020828403121560af575f5ffd5b503591905056")]
    contract ChaosTest {
        constructor() {}

        bytes32 constant FIB_BASE =
            0x0000000000000000000000000000000000000000000000000000000000000001;

        function fib(uint256 n) public returns (uint256 result) {
            assembly {
                sstore(FIB_BASE, 0)
                sstore(add(FIB_BASE, 1), 1)
                switch n
                case 0 {
                    result := 0
                }
                case 1 {
                    result := 1
                }
                default {
                    for {
                        let i := 2
                    } lt(i, add(n, 1)) {
                        i := add(i, 1)
                    } {
                        let fib_i_minus_1 := sload(add(FIB_BASE, sub(i, 1)))
                        let fib_i_minus_2 := sload(add(FIB_BASE, sub(i, 2)))
                        let fib_i := add(fib_i_minus_1, fib_i_minus_2)
                        sstore(add(FIB_BASE, i), fib_i)
                    }
                    result := sload(add(FIB_BASE, n))
                }
            }
        }
    }

    #[sol(bytecode = "0x6080604052348015600e575f5ffd5b50604051610223380380610223833981016040819052602b916051565b7f360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc55607c565b5f602082840312156060575f5ffd5b81516001600160a01b03811681146075575f5ffd5b9392505050565b61019a806100895f395ff3fe608060405260043610601e575f3560e01c8063b243cd76146067576024565b36602457005b7f360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc54365f5f375f5f365f845af490503d5f5f3e8080156061573d5ff35b3d5ffd5b005b3480156071575f5ffd5b5060655f604051607f9060bd565b604051809103905ff0801580156097573d5f5f3e3d5ffd5b507f360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc5550565b60d0806100ca8339019056fe6080604052348015600e575f5ffd5b5060b680601a5f395ff3fe6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063c6c2ea1714602a575b5f5ffd5b6039603536600460a0565b604b565b60405190815260200160405180910390f35b5f6001818155600255818015608e576001811460955760025b6001840181101560825780545f198201540160019091019081556064565b5082600101549150609a565b5f9150609a565b600191505b50919050565b5f6020828403121560af575f5ffd5b503591905056")]
    contract ChaosTestProxy {
        bytes32 constant IMPL_SLOT =
            0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc;

        constructor(address _implementation) {
            assembly {
                sstore(IMPL_SLOT, _implementation)
            }
        }

        function deployNewImplementation() external {
            ChaosTest newImpl = new ChaosTest();
            assembly {
                sstore(IMPL_SLOT, newImpl)
            }
        }

        fallback() external payable {
            assembly {
                let impl := sload(IMPL_SLOT)
                calldatacopy(0x00, 0x00, calldatasize())

                let success := delegatecall(
                    gas(),
                    impl,
                    0x00,
                    calldatasize(),
                    0x00,
                    0x00
                )

                returndatacopy(0x00, 0x00, returndatasize())

                switch success
                case 0 {
                    revert(0x00, returndatasize())
                }
                default {
                    return(0x00, returndatasize())
                }
            }
        }
        receive() external payable {}
    }
}

/// Target contract for chaos operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaosTarget {
    /// Direct call to ChaosTest contract
    Direct,
    /// Call via ChaosTestProxy (delegatecall)
    Proxy,
}

/// Represents different chaos operations that can be performed
#[derive(Debug, Clone)]
pub enum TxOp {
    /// Simple ETH transfer between accounts
    Transfer {
        from: PrivateKeySigner,
        to: Address,
        value: U256,
    },
    /// Call fib(n) on ChaosTest - creates sequential storage dependencies
    Fib {
        from: PrivateKeySigner,
        n: u64,
        target: ChaosTarget,
    },
    /// Call deployNewImplementation() on ChaosTestProxy - deploys new contract
    DeployNewImplementation { from: PrivateKeySigner },
}

impl TxOp {
    /// Returns the sender of this operation
    pub fn sender(&self) -> &PrivateKeySigner {
        match self {
            TxOp::Transfer { from, .. } => from,
            TxOp::Fib { from, .. } => from,
            TxOp::DeployNewImplementation { from } => from,
        }
    }

    /// Encodes the calldata for this operation
    pub fn encode_calldata(&self) -> Bytes {
        match self {
            TxOp::Transfer { .. } => Bytes::default(),
            TxOp::Fib { n, .. } => {
                // fib(uint256) selector from generated bindings
                ChaosTest::fibCall { n: U256::from(*n) }.abi_encode().into()
            }
            TxOp::DeployNewImplementation { .. } => {
                // deployNewImplementation() selector
                ChaosTestProxy::deployNewImplementationCall {}
                    .abi_encode()
                    .into()
            }
        }
    }

    /// Returns the target address for this operation
    pub fn target(&self) -> TxKind {
        match self {
            TxOp::Transfer { to, .. } => TxKind::Call(*to),
            TxOp::Fib { target, .. } => match target {
                ChaosTarget::Direct => TxKind::Call(*CHAOS_CONTRACT),
                ChaosTarget::Proxy => TxKind::Call(*CHAOS_PROXY_CONTRACT),
            },
            TxOp::DeployNewImplementation { .. } => TxKind::Call(*CHAOS_PROXY_CONTRACT),
        }
    }

    /// Returns the ETH value for this operation
    pub fn value(&self) -> U256 {
        match self {
            TxOp::Transfer { value, .. } => *value,
            _ => U256::ZERO,
        }
    }

    /// Returns estimated gas limit for this operation
    pub fn gas_limit(&self) -> u64 {
        match self {
            TxOp::Transfer { .. } => 21_000,
            TxOp::Fib { n, .. } => 100_000 + *n * 20_000,
            TxOp::DeployNewImplementation { .. } => 500_000,
        }
    }

    /// Creates a signed transaction from this operation
    pub fn to_signed_tx(&self, nonce: u64) -> Recovered<OpTransactionSigned> {
        let mut tx = TxEip1559 {
            chain_id: CHAIN_SPEC.chain().id(),
            nonce,
            max_fee_per_gas: CHAIN_SPEC.genesis_header().base_fee_per_gas.unwrap_or(1) as u128 * 2,
            max_priority_fee_per_gas: 1,
            to: self.target(),
            gas_limit: self.gas_limit(),
            value: self.value(),
            input: self.encode_calldata(),
            access_list: Default::default(),
        };

        let signed = self.sender().sign_transaction_sync(&mut tx).unwrap();
        let typed = OpTypedTransaction::Eip1559(tx);

        OpTxEnvelope::from((typed, signed))
            .try_clone_into_recovered()
            .unwrap()
    }

    /// Encodes transaction to bytes
    pub fn to_encoded_bytes(&self, nonce: u64) -> Bytes {
        let tx = self.to_signed_tx(nonce);
        let mut buf = Vec::new();
        tx.inner().encode_2718(&mut buf);
        Bytes::from(buf)
    }
}

/// Strategy for selecting a sender from test signers
pub fn arb_sender() -> impl Strategy<Value = PrivateKeySigner> {
    prop_oneof![
        Just(ALICE.clone()),
        Just(BOB.clone()),
        Just(CHARLIE.clone()),
    ]
}

/// Strategy for generating a single chaos operation
pub fn arb_transaction_op() -> impl Strategy<Value = TxOp> {
    prop_oneof![
        3 => (0usize..1usize).prop_map(|_| TxOp::Transfer { from: ALICE.clone(), to: Address::random(), value: U256::from(1_000_000_000u64) }),
        2 => (arb_sender(), 2u64..15u64, prop_oneof![Just(ChaosTarget::Direct), Just(ChaosTarget::Proxy),]).prop_map(|(from, n, target)| TxOp::Fib {
            from,
            n,
            target
        }),
        1 => arb_sender().prop_map(|from| TxOp::DeployNewImplementation { from }),
    ]
}

/// Strategy for generating a sequence of chaos operations with proper nonces
pub fn arb_transaction_sequence(max_ops: usize) -> impl Strategy<Value = Vec<(TxOp, u64)>> {
    prop::collection::vec(arb_transaction_op(), 1..=max_ops).prop_map(|ops| {
        let mut nonces: std::collections::HashMap<Address, u64> = std::collections::HashMap::new();

        ops.into_iter()
            .map(|op| {
                let sender_addr = op.sender().address();
                let nonce = *nonces.entry(sender_addr).or_insert(0);
                nonces.insert(sender_addr, nonce + 1);
                (op, nonce)
            })
            .collect()
    })
}

/// Converts a chaos sequence to signed transactions
pub fn transaction_op_sequence_to_transactions(
    sequence: &[(TxOp, u64)],
) -> Vec<Recovered<OpTransactionSigned>> {
    sequence
        .iter()
        .map(|(op, nonce)| op.to_signed_tx(*nonce))
        .collect()
}

/// Converts a chaos sequence to encoded transaction bytes
pub fn transaction_sequence_to_encoded(sequence: &[(TxOp, u64)]) -> Vec<Bytes> {
    sequence
        .iter()
        .map(|(op, nonce)| op.to_encoded_bytes(*nonce))
        .collect()
}

pub fn arb_execution_payload(
    max_ops_per_flashblock: usize,
) -> impl Strategy<
    Value = (
        ExecutionPayloadFlashblockDeltaV1,
        Option<CommittedState<OpRethReceiptBuilder>>,
    ),
> {
    arb_execution_payload_sequence(max_ops_per_flashblock, 1).prop_map(|mut v| v.pop().unwrap())
}

pub fn arb_execution_payload_sequence(
    transactions: usize,
    max_flashblocks: usize,
) -> impl Strategy<
    Value = Vec<(
        ExecutionPayloadFlashblockDeltaV1,
        Option<CommittedState<OpRethReceiptBuilder>>,
    )>,
> {
    // Generate a vector of chaos sequences (one per flashblock)
    arb_transaction_sequence(transactions).prop_filter_map(
        "execution must succeed",
        move |sequence: Vec<(TxOp, u64)>| build_chained_payloads(sequence, max_flashblocks).ok(),
    )
}

/// Builds a sequence of chained payloads from multiple tx sequences.
///
/// Each flashblock builds on the previous one's outcome, creating a realistic
/// sequence of incremental flashblock payloads within a single block.
pub fn build_chained_payloads(
    sequence: Vec<(TxOp, u64)>,
    max_flashblocks: usize,
) -> Result<
    Vec<(
        ExecutionPayloadFlashblockDeltaV1,
        Option<CommittedState<OpRethReceiptBuilder>>,
    )>,
    Box<dyn std::error::Error + Send + Sync>,
> {
    let mut payloads = Vec::with_capacity(max_flashblocks);
    let mut prev_outcome: Option<(BlockBuilderOutcome<OpPrimitives>, BundleState)> = None;

    // Ensure chunk_size is at least 1 to avoid panic
    let chunk_size = (sequence.len() / max_flashblocks).max(1);
    let sequences = sequence.chunks(chunk_size).collect::<Vec<_>>();

    for sequence in sequences.into_iter() {
        // Convert chaos ops to signed transactions
        let transactions = transaction_op_sequence_to_transactions(sequence);

        // Execute with the previous outcome for chaining
        let (outcome, bal_data, bundle_state) =
            execute_serial(prev_outcome.clone(), &transactions)?;

        // Encode transactions for the payload
        let encoded_txs = transaction_sequence_to_encoded(sequence);

        // Construct the payload
        let payload = ExecutionPayloadFlashblockDeltaV1 {
            state_root: outcome.block.state_root(),
            receipts_root: outcome.block.receipts_root(),
            logs_bloom: outcome.block.logs_bloom(),
            gas_used: outcome.block.gas_used(),
            block_hash: outcome.block.hash(),
            transactions: encoded_txs,
            withdrawals: vec![],
            withdrawals_root: outcome.block.withdrawals_root().unwrap_or_default(),
            access_list_data: Some(bal_data),
        };

        payloads.push((
            payload,
            prev_outcome.as_ref().map(|(o, state)| CommittedState {
                gas_used: o.block.gas_used(),
                fees: U256::ZERO,
                bundle: state.clone(),
                receipts: o
                    .execution_result
                    .receipts
                    .clone()
                    .iter()
                    .enumerate()
                    .map(|(idx, r)| (idx as u16, r.clone()))
                    .collect(),
                transactions: o
                    .block
                    .clone_transactions_recovered()
                    .enumerate()
                    .map(|(idx, tx)| (idx as u16, tx.clone()))
                    .collect(),
            }),
        ));

        // Store outcome for next iteration
        prev_outcome = Some((outcome, bundle_state));
    }

    Ok(payloads)
}

/// Creates a test database with genesis accounts
fn create_test_db() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    for (address, account) in GENESIS.alloc.clone() {
        db.insert_account_info(
            address,
            AccountInfo {
                balance: account.balance,
                nonce: account.nonce.unwrap_or_default(),
                code_hash: account
                    .code
                    .as_ref()
                    .map(alloy_primitives::keccak256)
                    .unwrap_or(KECCAK_EMPTY),
                code: account.code.map(revm::state::Bytecode::new_legacy),
            },
        );
    }
    db
}

/// Creates an Arc<dyn StateProvider> from an InMemoryDB
pub fn create_test_state_provider() -> Arc<TestStateProvider> {
    Arc::new(TestStateProvider::new())
}

/// Execute transactions sequentially and build a BAL
pub fn execute_serial(
    prev_outcome: Option<(BlockBuilderOutcome<OpPrimitives>, BundleState)>,
    transactions: &[Recovered<OpTransactionSigned>],
) -> Result<
    (
        BlockBuilderOutcome<OpPrimitives>,
        FlashblockAccessListData,
        BundleState,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let state_provider = create_test_state_provider();
    let db = StateProviderDatabase::new(state_provider.clone());

    let bundle = if let Some((_, bundle_state)) = &prev_outcome {
        bundle_state.clone()
    } else {
        BundleState::default()
    };

    let mut state = State::builder()
        .with_database(db.clone())
        .with_bundle_prestate(bundle.clone())
        .with_bundle_update()
        .build();

    let database = BalBuilderDb::new(&mut state);
    let prev_transaction = prev_outcome
        .as_ref()
        .map(|(o, _)| o.block.clone_transactions_recovered().collect());

    let evm = OpEvmFactory::default().create_evm(database, EVM_ENV.clone());

    let mut executor = OpBlockExecutor::new(
        evm,
        BLOCK_EXECUTION_CTX.clone(),
        CHAIN_SPEC.clone(),
        OpRethReceiptBuilder::default(),
    );

    executor.gas_used = prev_outcome.as_ref().map_or(0, |o| o.0.block.gas_used());
    executor.receipts = prev_outcome
        .as_ref()
        .map_or(Vec::new(), |o| o.0.execution_result.receipts.clone());

    let (access_list_tx, access_list_rx) = bounded(1);

    let mut builder = BalBlockBuilder::<OpRethReceiptBuilder, OpPrimitives, _>::new(
        BLOCK_EXECUTION_CTX.clone(),
        &SEALED_HEADER,
        executor,
        prev_transaction.unwrap_or_default(),
        CHAIN_SPEC.clone(),
        access_list_tx,
    );

    if prev_outcome.is_none() {
        builder.apply_pre_execution_changes()?;
    }

    for tx in transactions {
        builder.execute_transaction_with_commit_condition(tx.clone(), |_| CommitChanges::Yes)?;
    }

    let outcome = builder.finish(state_provider)?;

    let access_list = access_list_rx.recv()?;

    let hash = access_list_hash(&access_list);

    let bal_data = FlashblockAccessListData {
        access_list,
        access_list_hash: hash,
    };

    Ok((outcome, bal_data, state.take_bundle()))
}

// ============================================================================
// Mock State Provider with In-Memory Trie
// ============================================================================

/// Hashed genesis state for computing state roots.
/// This holds the initial account state in hashed form (keccak256(address) -> Account).
#[derive(Debug, Clone, Default)]
pub struct HashedGenesisState {
    /// Map of hashed address -> Account info
    pub accounts: HashMap<B256, Account>,
    /// Map of hashed address -> (Map of hashed slot -> value)
    pub storages: HashMap<B256, HashMap<B256, U256>>,
}

impl HashedGenesisState {
    /// Create a hashed genesis state from the genesis config
    pub fn from_genesis(genesis: &Genesis) -> Self {
        let mut accounts = HashMap::new();
        let mut storages = HashMap::new();

        for (address, account) in &genesis.alloc {
            let hashed_address = keccak256(address);

            // Convert to reth Account format
            let reth_account = Account {
                balance: account.balance,
                nonce: account.nonce.unwrap_or_default(),
                bytecode_hash: account.code.as_ref().map(|c| keccak256(c.as_ref())),
            };
            accounts.insert(hashed_address, reth_account);

            // Hash storage slots if any
            if let Some(storage) = &account.storage {
                let mut hashed_storage = HashMap::new();
                for (slot, value) in storage {
                    let hashed_slot = keccak256(slot);
                    // Convert B256 to U256 for storage values
                    hashed_storage.insert(hashed_slot, U256::from_be_bytes(value.0));
                }
                if !hashed_storage.is_empty() {
                    storages.insert(hashed_address, hashed_storage);
                }
            }
        }

        Self { accounts, storages }
    }

    /// Merge with a HashedPostState to get the final state
    /// Returns accounts and their storage for state root computation
    pub fn merge_with_post_state(
        &self,
        post_state: &HashedPostState,
    ) -> (HashMap<B256, Account>, HashMap<B256, HashMap<B256, U256>>) {
        // Start with genesis accounts
        let mut merged_accounts = self.accounts.clone();
        let mut merged_storages = self.storages.clone();

        // Apply account changes from post state
        for (hashed_address, account_opt) in &post_state.accounts {
            match account_opt {
                Some(account) => {
                    // Account was created or modified
                    merged_accounts.insert(*hashed_address, *account);
                }
                None => {
                    // Account was destroyed
                    merged_accounts.remove(hashed_address);
                    merged_storages.remove(hashed_address);
                }
            }
        }

        // Apply storage changes from post state
        for (hashed_address, hashed_storage) in &post_state.storages {
            let storage_map = merged_storages.entry(*hashed_address).or_default();

            // If storage was wiped, clear existing storage first
            if hashed_storage.wiped {
                storage_map.clear();
            }

            // Apply individual slot changes
            for (hashed_slot, value) in &hashed_storage.storage {
                if value.is_zero() {
                    storage_map.remove(hashed_slot);
                } else {
                    storage_map.insert(*hashed_slot, *value);
                }
            }
        }

        (merged_accounts, merged_storages)
    }

    /// Compute the state root from the merged state
    pub fn compute_state_root(
        accounts: &HashMap<B256, Account>,
        storages: &HashMap<B256, HashMap<B256, U256>>,
    ) -> B256 {
        use alloy_trie::root::{state_root_unsorted, storage_root_unsorted};

        // Build trie accounts with storage roots
        let trie_accounts: Vec<(B256, TrieAccount)> = accounts
            .iter()
            .map(|(hashed_address, account)| {
                // Compute storage root for this account
                let storage_root = if let Some(storage) = storages.get(hashed_address) {
                    if storage.is_empty() {
                        alloy_trie::EMPTY_ROOT_HASH
                    } else {
                        // Convert to sorted iterator for storage_root computation
                        let storage_iter: Vec<_> = storage.iter().map(|(k, v)| (*k, *v)).collect();
                        storage_root_unsorted(storage_iter)
                    }
                } else {
                    alloy_trie::EMPTY_ROOT_HASH
                };

                let trie_account = TrieAccount {
                    nonce: account.nonce,
                    balance: account.balance,
                    storage_root,
                    code_hash: account.bytecode_hash.unwrap_or(KECCAK_EMPTY),
                };

                (*hashed_address, trie_account)
            })
            .collect();

        if trie_accounts.is_empty() {
            alloy_trie::EMPTY_ROOT_HASH
        } else {
            state_root_unsorted(trie_accounts)
        }
    }
}

/// Test state provider backed by InMemoryDB with in-memory trie for state root computation
pub struct TestStateProvider {
    db: InMemoryDB,
    /// Hashed genesis state for computing state roots
    hashed_genesis: HashedGenesisState,
}

impl TestStateProvider {
    pub fn new() -> Self {
        Self {
            db: create_test_db(),
            hashed_genesis: HashedGenesisState::from_genesis(&GENESIS),
        }
    }
}

impl BytecodeReader for TestStateProvider {
    fn bytecode_by_hash(
        &self,
        code_hash: &alloy_primitives::B256,
    ) -> reth_provider::ProviderResult<Option<reth_primitives::Bytecode>> {
        use revm::DatabaseRef;
        Ok(self
            .db
            .code_by_hash_ref(*code_hash)
            .ok()
            .map(|bc| reth_primitives::Bytecode::new_raw(bc.original_bytes())))
    }
}

impl StateProvider for TestStateProvider {
    fn storage(
        &self,
        account: Address,
        storage_key: alloy_primitives::StorageKey,
    ) -> reth_provider::ProviderResult<Option<alloy_primitives::StorageValue>> {
        Ok(self.db.storage_ref(account, storage_key.into()).ok())
    }
}

impl reth_provider::AccountReader for TestStateProvider {
    fn basic_account(
        &self,
        address: &Address,
    ) -> reth_provider::ProviderResult<Option<reth_primitives::Account>> {
        Ok(self
            .db
            .basic_ref(*address)
            .ok()
            .flatten()
            .map(|info| reth_primitives::Account {
                balance: info.balance,
                nonce: info.nonce,
                bytecode_hash: if info.code_hash == KECCAK_EMPTY {
                    None
                } else {
                    Some(info.code_hash)
                },
            }))
    }
}

impl reth_provider::BlockHashReader for TestStateProvider {
    fn block_hash(
        &self,
        number: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<Option<alloy_primitives::B256>> {
        if number == 0 {
            Ok(Some(CHAIN_SPEC.genesis_hash()))
        } else {
            Ok(None)
        }
    }

    fn canonical_hashes_range(
        &self,
        _start: alloy_primitives::BlockNumber,
        _end: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<Vec<alloy_primitives::B256>> {
        Ok(vec![])
    }
}

impl reth_provider::StateRootProvider for TestStateProvider {
    fn state_root(
        &self,
        hashed_state: reth_trie_common::HashedPostState,
    ) -> reth_provider::ProviderResult<B256> {
        let (accounts, storages) = self.hashed_genesis.merge_with_post_state(&hashed_state);
        Ok(HashedGenesisState::compute_state_root(&accounts, &storages))
    }

    fn state_root_from_nodes(
        &self,
        _input: reth_trie_common::TrieInput,
    ) -> reth_provider::ProviderResult<B256> {
        // For testing, just return empty root
        Ok(alloy_trie::EMPTY_ROOT_HASH)
    }

    fn state_root_with_updates(
        &self,
        hashed_state: reth_trie_common::HashedPostState,
    ) -> reth_provider::ProviderResult<(B256, reth_trie_common::updates::TrieUpdates)> {
        let (accounts, storages) = self.hashed_genesis.merge_with_post_state(&hashed_state);
        let state_root = HashedGenesisState::compute_state_root(&accounts, &storages);
        // For testing, we don't need actual trie updates
        Ok((
            state_root,
            reth_trie_common::updates::TrieUpdates::default(),
        ))
    }

    fn state_root_from_nodes_with_updates(
        &self,
        _input: reth_trie_common::TrieInput,
    ) -> reth_provider::ProviderResult<(B256, reth_trie_common::updates::TrieUpdates)> {
        // For testing, just return empty root
        Ok((
            alloy_trie::EMPTY_ROOT_HASH,
            reth_trie_common::updates::TrieUpdates::default(),
        ))
    }
}

impl reth_provider::StorageRootProvider for TestStateProvider {
    fn storage_root(
        &self,
        address: Address,
        hashed_storage: reth_trie_common::HashedStorage,
    ) -> reth_provider::ProviderResult<B256> {
        use alloy_trie::root::storage_root_unsorted;

        let hashed_address = keccak256(address);

        // Start with genesis storage for this account
        let mut storage = self
            .hashed_genesis
            .storages
            .get(&hashed_address)
            .cloned()
            .unwrap_or_default();

        // Apply changes from hashed_storage
        if hashed_storage.wiped {
            storage.clear();
        }
        for (hashed_slot, value) in &hashed_storage.storage {
            if value.is_zero() {
                storage.remove(hashed_slot);
            } else {
                storage.insert(*hashed_slot, *value);
            }
        }

        if storage.is_empty() {
            Ok(alloy_trie::EMPTY_ROOT_HASH)
        } else {
            let storage_iter: Vec<_> = storage.into_iter().collect();
            Ok(storage_root_unsorted(storage_iter))
        }
    }

    fn storage_proof(
        &self,
        _address: Address,
        _slot: B256,
        _hashed_storage: reth_trie_common::HashedStorage,
    ) -> reth_provider::ProviderResult<reth_trie_common::StorageProof> {
        unimplemented!()
    }

    fn storage_multiproof(
        &self,
        _address: Address,
        _slots: &[B256],
        _hashed_storage: reth_trie_common::HashedStorage,
    ) -> reth_provider::ProviderResult<reth_trie_common::StorageMultiProof> {
        unimplemented!()
    }
}

impl reth_provider::StateProofProvider for TestStateProvider {
    fn proof(
        &self,
        _input: reth_trie_common::TrieInput,
        _address: Address,
        _slots: &[alloy_primitives::B256],
    ) -> reth_provider::ProviderResult<reth_trie_common::AccountProof> {
        unimplemented!()
    }

    fn multiproof(
        &self,
        _input: reth_trie_common::TrieInput,
        _targets: reth_trie_common::MultiProofTargets,
    ) -> reth_provider::ProviderResult<reth_trie_common::MultiProof> {
        unimplemented!()
    }

    fn witness(
        &self,
        _input: reth_trie_common::TrieInput,
        _target: reth_trie_common::HashedPostState,
    ) -> reth_provider::ProviderResult<Vec<Bytes>> {
        unimplemented!()
    }
}

impl reth_provider::HashedPostStateProvider for TestStateProvider {
    fn hashed_post_state(&self, bundle_state: &BundleState) -> reth_trie_common::HashedPostState {
        reth_trie_common::HashedPostState::from_bundle_state::<reth_trie_common::KeccakKeyHasher>(
            bundle_state.state(),
        )
    }
}
