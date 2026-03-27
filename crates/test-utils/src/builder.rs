//! Test utilities for world-chain-builder benchmarks and integration tests.
//!
//! This module provides shared fixture data, mock providers, and helper
//! functions for constructing realistic flashblock payloads.  It was extracted
//! from `world-chain-builder`'s internal `test_utils` module so that
//! downstream test and fuzz crates can depend on `world-chain-test-utils` alone.

use alloy_consensus::{BlockHeader, TxEip1559, constants::KECCAK_EMPTY};
use alloy_eips::{BlockNumHash, eip2718::Encodable2718};
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor, OpEvmFactory};
use alloy_primitives::{
    Address, B256, Bytes, FixedBytes, StorageKey, TxKind, U256, bytes, hex, keccak256,
};
use alloy_rpc_types_engine::PayloadId;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, sol};
use alloy_trie::TrieAccount;
use crossbeam_channel::bounded;
use op_alloy_consensus::{OpTxEnvelope, OpTypedTransaction};
use op_alloy_network::TxSignerSync;
use reth::revm::{State, database::StateProviderDatabase};
use reth_chainspec::ChainInfo;
use reth_evm::{
    ConfigureEvm, EvmEnv, EvmFactory,
    execute::{BlockBuilder, BlockBuilderOutcome},
    op_revm::OpSpecId,
};
use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
use reth_optimism_evm::{OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder};
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives::{Account, Recovered, SealedHeader, transaction::SignedTransaction};
use reth_provider::{
    BlockIdReader, BlockNumReader, BytecodeReader, CanonStateSubscriptions, ChainSpecProvider,
    HeaderProvider, NodePrimitivesProvider, ProviderResult, StateProvider, StateProviderBox,
    StateProviderFactory,
};
use reth_trie_common::HashedPostState;
use revm::{
    DatabaseRef,
    database::{BundleState, InMemoryDB},
    primitives::StorageValue,
    state::AccountInfo,
};
use std::{
    collections::{BTreeMap, HashMap},
    ops::RangeBounds,
    sync::Arc,
};
use tracing::error;
use primitives::{
    access_list::{FlashblockAccessListData, access_list_hash},
    primitives::{ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1},
};

use builder::{
    BlockBuilderExt,
    bal_executor::{BalBlockBuilder, CommittedState},
    database::bal_builder_db::BalBuilderDb,
};

const WORLD_ID_ALT_INPUT_INTERVAL: usize = 5;
const WORLD_ID_TREE_DEPTH: u64 = 30;
const WORLD_ID_ISSUER_SCHEMA_ID: u64 = 1;
const WORLD_ID_RP_ID: u64 = 0x1a6ccf8f70e5de68;
const WORLD_ID_ZK_GAS_LIMIT: u64 = 1_000_000;

#[derive(Debug, Clone, Copy)]
pub struct WorldIdBenchCase {
    compressed_proof: [U256; 4],
    public_inputs: [U256; 15],
}

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

    /// World ID Groth16 verifier contract address (deterministic)
    pub static ref WORLD_ID_VERIFIER_CONTRACT: Address =
        Address::from_slice(&[0xCA, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03]);

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

    /// Deployed runtime bytecode for world-id-protocol's `Verifier.sol`.
    pub static ref WORLD_ID_VERIFIER_RUNTIME_BYTECODE: Bytes = {
        let hex = include_str!("../res/world_id_verifier_runtime.hex");
        let normalized: String = hex.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        Bytes::from(
            hex::decode(normalized.trim_start_matches("0x"))
                .expect("valid World ID verifier runtime bytecode hex")
        )
    };

    /// Real compressed proof vectors sourced from `world-id-protocol` tests.
    static ref WORLD_ID_BENCH_CASES: Vec<WorldIdBenchCase> = vec![
        WorldIdBenchCase {
            compressed_proof: [
                u256_from_hex("4906f4e17b969ef2cfc44bd96520f01a3f5c32972bca2e10b70e05e03e3d9f13"),
                u256_from_hex("0d6d9a3456e9af7d8f6f78eb3380deb8c93505c062f62fa18b8ef8a2ccb55db8"),
                u256_from_hex("a92a48edeb327b190048648788de9a8eff0abed5dc93bee8881387da40571278"),
                u256_from_hex("38f52985c393efb732be8f54b5f00f7f25370ac5945de84e0d8d2f2d298866b8"),
            ],
            public_inputs: [
                u256_from_hex("1bae01b23e5f0ee96151331fffb0550351c52e5ee0ced452c762e120723ae702"),
                U256::from(WORLD_ID_ISSUER_SCHEMA_ID),
                u256_from_hex("252c8234509649bb469ecb7a7e758f306b41415f2d80d4d67967902d6f589a81"),
                u256_from_hex("230e4f93a5f1187639314dd25e595db06dc18de219cfaeb8cfdf81d4afe910d5"),
                u256_from_hex("699cfa47"),
                U256::ZERO,
                u256_from_hex("0af727d9412a9d5c73b685fd09dc39e727064e65b8269b233009edfc105f9853"),
                U256::from(WORLD_ID_TREE_DEPTH),
                U256::from(WORLD_ID_RP_ID),
                u256_from_hex("15d4b66e5417cb9875f6a2b5be9814dca80651d7c74b3b21685fdd494566e79f"),
                u256_from_hex("0ac79da013272129ddceae6d20c0f579abd04b0a00160ed2be2151bf4014e8d"),
                u256_from_hex("187ce5ac507fe0760e95d1893cc6ebf3a115eb9adeaa355c14cc52722a2275be"),
                u256_from_hex("1578ed0de47522ad0b38e87031739c6a65caecc39ce3410bf3799e756a220f"),
                u256_from_hex("18e3ab3d5fedc6eaa5e0d06a3a6f3dd5e0bf2d17b18b797a1cc6ff4706169d1e"),
                U256::ZERO,
            ],
        },
        WorldIdBenchCase {
            compressed_proof: [
                u256_from_hex("4533f8d38447da676c8eac8ec01ce031af1cc140d8397f3baf792be414c28790"),
                u256_from_hex("0e05c9ada0f2a3ebb5863f0a3412aa852cea67099ce26bb46c44b264af5b6927"),
                u256_from_hex("178bbfe59fc10b5ec4359ecb21b9f42fb8afef08e90cd3dec903fdd45cddc930"),
                u256_from_hex("409b8908726ca9151d021fcecc882a3f5e93ba35f6043ad0bd51258b55e5018b"),
            ],
            public_inputs: [
                u256_from_hex("1bae01b23e5f0ee96151331fffb0550351c52e5ee0ced452c762e120723ae702"),
                U256::from(WORLD_ID_ISSUER_SCHEMA_ID),
                u256_from_hex("252c8234509649bb469ecb7a7e758f306b41415f2d80d4d67967902d6f589a81"),
                u256_from_hex("230e4f93a5f1187639314dd25e595db06dc18de219cfaeb8cfdf81d4afe910d5"),
                u256_from_hex("699cfa47"),
                U256::ZERO,
                u256_from_hex("0af727d9412a9d5c73b685fd09dc39e727064e65b8269b233009edfc105f9853"),
                U256::from(WORLD_ID_TREE_DEPTH),
                U256::from(WORLD_ID_RP_ID),
                u256_from_hex("15d4b66e5417cb9875f6a2b5be9814dca80651d7c74b3b21685fdd494566e79f"),
                u256_from_hex("0ac79da013272129ddceae6d20c0f579abd04b0a00160ed2be2151bf4014e8d"),
                u256_from_hex("187ce5ac507fe0760e95d1893cc6ebf3a115eb9adeaa355c14cc52722a2275be"),
                u256_from_hex("1578ed0de47522ad0b38e87031739c6a65caecc39ce3410bf3799e756a220f"),
                u256_from_hex("18e3ab3d5fedc6eaa5e0d06a3a6f3dd5e0bf2d17b18b797a1cc6ff4706169d1e"),
                u256_from_hex("2025d8e786806a895f7e50ce403f7d6e33e501772b28116908ad6fa5108172f8"),
            ],
        },
    ];

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
            (
                *WORLD_ID_VERIFIER_CONTRACT,
                GenesisAccount::default()
                    .with_code(Some(WORLD_ID_VERIFIER_RUNTIME_BYTECODE.clone()))
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
        extra_data: bytes!("0x000000000800000002")
    };

    pub static ref NEXT_BLOCK_ENV_ATTRS: OpNextBlockEnvAttributes = OpNextBlockEnvAttributes {
        timestamp: CHAIN_SPEC.genesis_header().timestamp() + 1,
        suggested_fee_recipient: Address::ZERO,
        prev_randao: FixedBytes::ZERO,
        gas_limit: CHAIN_SPEC.genesis_header().gas_limit,
        parent_beacon_block_root: Some(FixedBytes::ZERO),
        extra_data: bytes!("0x000000000800000002"),
    };

    pub static ref SEALED_HEADER: SealedHeader = SealedHeader::seal_slow(CHAIN_SPEC.genesis_header().clone());

    pub static ref EVM_ENV: EvmEnv<OpSpecId> = EVM_CONFIG
        .next_evm_env(SEALED_HEADER.header(), &NEXT_BLOCK_ENV_ATTRS)
        .unwrap();
}

fn u256_from_hex(value: &str) -> U256 {
    U256::from_str_radix(value.trim_start_matches("0x"), 16).expect("valid U256 hex")
}

sol! {
    #[sol(bytecode = "0x6080604052348015600e575f5ffd5b5060b680601a5f395ff3fe6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063c6c2ea1714602a575b5f5ffd5b6039603536600460a0565b604b565b60405190815260200160405180910390f35b5f6001818155600255818015608e576001811460955760025b6001840181101560825780545f198201540160019091019081556064565b5082600101549150609a565b5f9150609a565b600191505b50919050565b5f6020828403121560af575f5ffd5b503591905056")]
    contract Fib {
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
    contract FibProxy {
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

    contract WorldIdGroth16Verifier {
        function verifyCompressedProof(
            uint256[4] calldata compressedProof,
            uint256[15] calldata input
        ) external view;
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
    /// Call the real World ID verifier hot path using compressed Groth16 proofs.
    WorldIdVerifyCompressed {
        from: PrivateKeySigner,
        case: Box<WorldIdBenchCase>,
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
            TxOp::WorldIdVerifyCompressed { from, .. } => from,
            TxOp::DeployNewImplementation { from } => from,
        }
    }

    /// Encodes the calldata for this operation
    pub fn encode_calldata(&self) -> Bytes {
        match self {
            TxOp::Transfer { .. } => Bytes::default(),
            TxOp::Fib { n, .. } => {
                // fib(uint256) selector from generated bindings
                Fib::fibCall { n: U256::from(*n) }.abi_encode().into()
            }
            TxOp::WorldIdVerifyCompressed { case, .. } => {
                WorldIdGroth16Verifier::verifyCompressedProofCall {
                    compressedProof: case.compressed_proof,
                    input: case.public_inputs,
                }
                .abi_encode()
                .into()
            }
            TxOp::DeployNewImplementation { .. } => {
                // deployNewImplementation() selector
                FibProxy::deployNewImplementationCall {}.abi_encode().into()
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
            TxOp::WorldIdVerifyCompressed { .. } => TxKind::Call(*WORLD_ID_VERIFIER_CONTRACT),
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
            TxOp::WorldIdVerifyCompressed { .. } => WORLD_ID_ZK_GAS_LIMIT,
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

/// Builds a sequence of chained payloads from multiple tx sequences.
///
/// Each flashblock builds on the previous one's outcome, creating a realistic
/// sequence of incremental flashblock payloads within a single block.
pub fn build_chained_payloads(
    sequence: Vec<(TxOp, u64)>,
    max_flashblocks: usize,
    bal: bool,
) -> Result<
    Vec<(
        ExecutionPayloadFlashblockDeltaV1,
        Option<CommittedState<OpRethReceiptBuilder>>,
    )>,
    Box<dyn std::error::Error + Send + Sync>,
> {
    let mut payloads = Vec::with_capacity(max_flashblocks);
    let mut prev_outcome: Option<(BlockBuilderOutcome<OpPrimitives>, BundleState)> = None;

    // Split the sequence into determistic chunks - each chunk represents a flashblock
    let chunk_size = (sequence.len() / max_flashblocks).max(1);
    let sequences = sequence.chunks(chunk_size).collect::<Vec<_>>();

    for sequence in sequences.into_iter() {
        // Convert transaction ops to signed transactions
        let transactions = transaction_op_sequence_to_transactions(sequence);

        // Execute over the previous outcome, if any
        let (outcome, bal_data, bundle_state) =
            execute_serial(prev_outcome.clone(), &transactions)?;

        for receipt in outcome.execution_result.receipts.iter() {
            if !receipt.as_receipt().status.coerce_status() {
                error!(
                    "Transaction failed in execution: {:?}",
                    receipt.as_receipt()
                );

                return Err(format!(
                    "Transaction failed in execution: {:?}",
                    receipt.as_receipt()
                )
                .into());
            }
        }
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
            access_list_data: if bal { Some(bal_data) } else { None },
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

/// Executes a series of transactions serially, building on the previous outcome.
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
    let db = StateProviderDatabase::new(state_provider.as_ref());

    let bundle = if let Some((_, bundle_state)) = &prev_outcome {
        bundle_state.clone()
    } else {
        BundleState::default()
    };

    let mut state = State::builder()
        .with_database(db)
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
        builder.execute_transaction_with_result_closure(tx.clone(), |_| {})?;
    }

    let (outcome, bundle_state) = builder.finish_with_bundle(state_provider.as_ref(), None)?;

    let access_list = access_list_rx.recv()?;

    let hash = access_list_hash(&access_list);

    let bal_data = FlashblockAccessListData {
        access_list,
        access_list_hash: hash,
    };

    Ok((outcome, bal_data, bundle_state))
}

// ============================================================================
// Mock State Provider with In-Memory Trie
// ============================================================================
fn create_test_db() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    for (address, account) in GENESIS.alloc.clone() {
        db.insert_account_info(
            address,
            AccountInfo {
                account_id: None,
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
#[derive(Debug, Clone)]
pub struct TestStateProvider {
    db: InMemoryDB,
    /// Hashed genesis state for computing state roots
    hashed_genesis: HashedGenesisState,
}

impl Default for TestStateProvider {
    fn default() -> Self {
        Self::new()
    }
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
        storage_key: StorageKey,
    ) -> reth_provider::ProviderResult<Option<alloy_primitives::StorageValue>> {
        Ok(self.db.storage_ref(account, storage_key.into()).ok())
    }

    fn storage_by_hashed_key(
        &self,
        _address: Address,
        _hashed_storage_key: StorageKey,
    ) -> ProviderResult<Option<StorageValue>> {
        todo!()
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

impl BlockNumReader for TestStateProvider {
    fn chain_info(&self) -> reth_provider::ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: CHAIN_SPEC.genesis_hash(),
            best_number: 0,
        })
    }

    fn best_block_number(&self) -> reth_provider::ProviderResult<alloy_primitives::BlockNumber> {
        Ok(0)
    }

    fn last_block_number(&self) -> reth_provider::ProviderResult<alloy_primitives::BlockNumber> {
        Ok(0)
    }

    fn block_number(
        &self,
        hash: alloy_primitives::B256,
    ) -> reth_provider::ProviderResult<Option<alloy_primitives::BlockNumber>> {
        if hash == CHAIN_SPEC.genesis_hash() {
            Ok(Some(0))
        } else {
            Ok(None)
        }
    }
}

impl BlockIdReader for TestStateProvider {
    fn pending_block_num_hash(&self) -> reth_provider::ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash {
            number: 0,
            hash: CHAIN_SPEC.genesis_hash(),
        }))
    }

    fn safe_block_num_hash(&self) -> reth_provider::ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash {
            number: 0,
            hash: CHAIN_SPEC.genesis_hash(),
        }))
    }

    fn finalized_block_num_hash(&self) -> reth_provider::ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash {
            number: 0,
            hash: CHAIN_SPEC.genesis_hash(),
        }))
    }
}

impl StateProviderFactory for TestStateProvider {
    fn latest(&self) -> reth_provider::ProviderResult<StateProviderBox> {
        Ok(Box::new(self.clone()))
    }

    fn state_by_block_number_or_tag(
        &self,
        _number_or_tag: alloy_eips::BlockNumberOrTag,
    ) -> reth_provider::ProviderResult<StateProviderBox> {
        Ok(Box::new(self.clone()))
    }

    fn history_by_block_number(
        &self,
        _block: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<StateProviderBox> {
        Ok(Box::new(self.clone()))
    }

    fn history_by_block_hash(
        &self,
        _block: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<StateProviderBox> {
        Ok(Box::new(self.clone()))
    }

    fn state_by_block_hash(
        &self,
        _block: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<StateProviderBox> {
        Ok(Box::new(self.clone()))
    }

    fn pending(&self) -> reth_provider::ProviderResult<StateProviderBox> {
        Ok(Box::new(self.clone()))
    }

    fn pending_state_by_hash(
        &self,
        _block_hash: B256,
    ) -> reth_provider::ProviderResult<Option<StateProviderBox>> {
        Ok(Some(Box::new(self.clone())))
    }

    fn maybe_pending(&self) -> reth_provider::ProviderResult<Option<StateProviderBox>> {
        Ok(Some(Box::new(self.clone())))
    }
}

// ============================================================================
// BenchProvider — wraps TestStateProvider and adds HeaderProvider + ChainSpecProvider
// ============================================================================

/// Provider wrapper that adds [`HeaderProvider`] and [`ChainSpecProvider`]
/// implementations needed by [`process_flashblock`](builder::coordinator::process_flashblock).
#[derive(Debug, Clone)]
pub struct BenchProvider {
    pub inner: TestStateProvider,
    pub sealed_header: SealedHeader,
    pub chain_spec: Arc<OpChainSpec>,
}

impl Default for BenchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BenchProvider {
    pub fn new() -> Self {
        Self {
            inner: TestStateProvider::new(),
            sealed_header: SEALED_HEADER.clone(),
            chain_spec: CHAIN_SPEC.clone(),
        }
    }
}

// Delegate all TestStateProvider traits to self.inner

impl BytecodeReader for BenchProvider {
    fn bytecode_by_hash(
        &self,
        code_hash: &B256,
    ) -> ProviderResult<Option<reth_primitives::Bytecode>> {
        self.inner.bytecode_by_hash(code_hash)
    }
}

impl StateProvider for BenchProvider {
    fn storage(
        &self,
        account: Address,
        storage_key: StorageKey,
    ) -> ProviderResult<Option<alloy_primitives::StorageValue>> {
        self.inner.storage(account, storage_key)
    }

    fn storage_by_hashed_key(
        &self,
        address: Address,
        hashed_storage_key: StorageKey,
    ) -> ProviderResult<Option<StorageValue>> {
        self.inner
            .storage_by_hashed_key(address, hashed_storage_key)
    }
}

impl reth_provider::AccountReader for BenchProvider {
    fn basic_account(&self, address: &Address) -> ProviderResult<Option<Account>> {
        self.inner.basic_account(address)
    }
}

impl reth_provider::BlockHashReader for BenchProvider {
    fn block_hash(&self, number: alloy_primitives::BlockNumber) -> ProviderResult<Option<B256>> {
        self.inner.block_hash(number)
    }

    fn canonical_hashes_range(
        &self,
        start: alloy_primitives::BlockNumber,
        end: alloy_primitives::BlockNumber,
    ) -> ProviderResult<Vec<B256>> {
        self.inner.canonical_hashes_range(start, end)
    }
}

impl reth_provider::StateRootProvider for BenchProvider {
    fn state_root(&self, hashed_state: HashedPostState) -> ProviderResult<B256> {
        self.inner.state_root(hashed_state)
    }

    fn state_root_from_nodes(&self, input: reth_trie_common::TrieInput) -> ProviderResult<B256> {
        self.inner.state_root_from_nodes(input)
    }

    fn state_root_with_updates(
        &self,
        hashed_state: HashedPostState,
    ) -> ProviderResult<(B256, reth_trie_common::updates::TrieUpdates)> {
        self.inner.state_root_with_updates(hashed_state)
    }

    fn state_root_from_nodes_with_updates(
        &self,
        input: reth_trie_common::TrieInput,
    ) -> ProviderResult<(B256, reth_trie_common::updates::TrieUpdates)> {
        self.inner.state_root_from_nodes_with_updates(input)
    }
}

impl reth_provider::StorageRootProvider for BenchProvider {
    fn storage_root(
        &self,
        address: Address,
        hashed_storage: reth_trie_common::HashedStorage,
    ) -> ProviderResult<B256> {
        self.inner.storage_root(address, hashed_storage)
    }

    fn storage_proof(
        &self,
        address: Address,
        slot: B256,
        hashed_storage: reth_trie_common::HashedStorage,
    ) -> ProviderResult<reth_trie_common::StorageProof> {
        self.inner.storage_proof(address, slot, hashed_storage)
    }

    fn storage_multiproof(
        &self,
        address: Address,
        slots: &[B256],
        hashed_storage: reth_trie_common::HashedStorage,
    ) -> ProviderResult<reth_trie_common::StorageMultiProof> {
        self.inner
            .storage_multiproof(address, slots, hashed_storage)
    }
}

impl reth_provider::StateProofProvider for BenchProvider {
    fn proof(
        &self,
        input: reth_trie_common::TrieInput,
        address: Address,
        slots: &[B256],
    ) -> ProviderResult<reth_trie_common::AccountProof> {
        self.inner.proof(input, address, slots)
    }

    fn multiproof(
        &self,
        input: reth_trie_common::TrieInput,
        targets: reth_trie_common::MultiProofTargets,
    ) -> ProviderResult<reth_trie_common::MultiProof> {
        self.inner.multiproof(input, targets)
    }

    fn witness(
        &self,
        input: reth_trie_common::TrieInput,
        target: reth_trie_common::HashedPostState,
    ) -> ProviderResult<Vec<Bytes>> {
        self.inner.witness(input, target)
    }
}

impl reth_provider::HashedPostStateProvider for BenchProvider {
    fn hashed_post_state(&self, bundle_state: &BundleState) -> HashedPostState {
        self.inner.hashed_post_state(bundle_state)
    }
}

impl BlockNumReader for BenchProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        self.inner.chain_info()
    }

    fn best_block_number(&self) -> ProviderResult<alloy_primitives::BlockNumber> {
        self.inner.best_block_number()
    }

    fn last_block_number(&self) -> ProviderResult<alloy_primitives::BlockNumber> {
        self.inner.last_block_number()
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<alloy_primitives::BlockNumber>> {
        self.inner.block_number(hash)
    }
}

impl BlockIdReader for BenchProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        self.inner.pending_block_num_hash()
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        self.inner.safe_block_num_hash()
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        self.inner.finalized_block_num_hash()
    }
}

impl StateProviderFactory for BenchProvider {
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        self.inner.latest()
    }

    fn state_by_block_number_or_tag(
        &self,
        number_or_tag: alloy_eips::BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        self.inner.state_by_block_number_or_tag(number_or_tag)
    }

    fn history_by_block_number(
        &self,
        block: alloy_primitives::BlockNumber,
    ) -> ProviderResult<StateProviderBox> {
        self.inner.history_by_block_number(block)
    }

    fn history_by_block_hash(
        &self,
        block: alloy_primitives::BlockHash,
    ) -> ProviderResult<StateProviderBox> {
        self.inner.history_by_block_hash(block)
    }

    fn state_by_block_hash(
        &self,
        block: alloy_primitives::BlockHash,
    ) -> ProviderResult<StateProviderBox> {
        self.inner.state_by_block_hash(block)
    }

    fn pending(&self) -> ProviderResult<StateProviderBox> {
        self.inner.pending()
    }

    fn pending_state_by_hash(&self, block_hash: B256) -> ProviderResult<Option<StateProviderBox>> {
        self.inner.pending_state_by_hash(block_hash)
    }

    fn maybe_pending(&self) -> ProviderResult<Option<StateProviderBox>> {
        self.inner.maybe_pending()
    }
}

// HeaderProvider — returns the genesis sealed header when the hash matches
impl HeaderProvider for BenchProvider {
    type Header = alloy_consensus::Header;

    fn header(
        &self,
        block_hash: alloy_primitives::BlockHash,
    ) -> ProviderResult<Option<alloy_consensus::Header>> {
        if block_hash == self.sealed_header.hash() {
            Ok(Some(self.sealed_header.header().clone()))
        } else {
            Ok(None)
        }
    }

    fn header_by_number(&self, num: u64) -> ProviderResult<Option<alloy_consensus::Header>> {
        if num == 0 {
            Ok(Some(self.sealed_header.header().clone()))
        } else {
            Ok(None)
        }
    }

    fn headers_range(
        &self,
        range: impl RangeBounds<alloy_primitives::BlockNumber>,
    ) -> ProviderResult<Vec<alloy_consensus::Header>> {
        if range.contains(&0) {
            Ok(vec![self.sealed_header.header().clone()])
        } else {
            Ok(vec![])
        }
    }

    fn sealed_header(
        &self,
        number: alloy_primitives::BlockNumber,
    ) -> ProviderResult<Option<SealedHeader>> {
        if number == 0 {
            Ok(Some(self.sealed_header.clone()))
        } else {
            Ok(None)
        }
    }

    fn sealed_headers_while(
        &self,
        range: impl RangeBounds<alloy_primitives::BlockNumber>,
        mut predicate: impl FnMut(&SealedHeader) -> bool,
    ) -> ProviderResult<Vec<SealedHeader>> {
        if range.contains(&0) && predicate(&self.sealed_header) {
            Ok(vec![self.sealed_header.clone()])
        } else {
            Ok(vec![])
        }
    }
}

// ChainSpecProvider — returns the test chain spec
impl ChainSpecProvider for BenchProvider {
    type ChainSpec = OpChainSpec;

    fn chain_spec(&self) -> Arc<OpChainSpec> {
        self.chain_spec.clone()
    }
}

// NodePrimitivesProvider — required supertrait for CanonStateSubscriptions
impl NodePrimitivesProvider for BenchProvider {
    type Primitives = OpPrimitives;
}

// CanonStateSubscriptions — noop: benchmarks never receive canon state notifications
impl CanonStateSubscriptions for BenchProvider {
    fn subscribe_to_canonical_state(
        &self,
    ) -> reth_chain_state::CanonStateNotifications<Self::Primitives> {
        tokio::sync::broadcast::channel(1).1
    }
}

pub fn build_flashblock_fixture_eth_transfers(num_txs: usize, bal: bool) -> FlashblocksPayloadV1 {
    build_flashblock_fixture(num_txs, bal, || TxOp::Transfer {
        from: ALICE.clone(),
        to: Address::random(),
        value: U256::from(100),
    })
}

pub fn build_flashblock_fixture_fib(num_txs: usize, bal: bool) -> FlashblocksPayloadV1 {
    build_flashblock_fixture(num_txs, bal, || TxOp::Fib {
        from: ALICE.clone(),
        n: 300,
        target: ChaosTarget::Proxy,
    })
}

pub fn build_flashblock_fixture_world_id_like_bn254(
    num_txs: usize,
    bal: bool,
) -> FlashblocksPayloadV1 {
    build_flashblock_fixture_from_sequence(
        build_world_id_bench_transaction_sequence(num_txs),
        bal,
        PayloadId::new([3u8; 8]),
    )
}

fn benchmark_base_payload() -> ExecutionPayloadBaseV1 {
    ExecutionPayloadBaseV1 {
        parent_hash: CHAIN_SPEC.genesis_hash(),
        parent_beacon_block_root: B256::ZERO,
        fee_recipient: Address::ZERO,
        prev_randao: B256::ZERO,
        block_number: 1,
        gas_limit: CHAIN_SPEC.genesis_header().gas_limit,
        timestamp: CHAIN_SPEC.genesis_header().timestamp() + 1,
        extra_data: bytes!("0x000000000800000002"),
        base_fee_per_gas: U256::from(CHAIN_SPEC.genesis_header().base_fee_per_gas.unwrap_or(1)),
    }
}

fn build_flashblock_fixture_from_sequence(
    sequence: Vec<(TxOp, u64)>,
    bal: bool,
    payload_id: PayloadId,
) -> FlashblocksPayloadV1 {
    let payloads =
        build_chained_payloads(sequence, 1, bal).expect("failed to build chained payloads");
    let (diff, _committed_state) = payloads.into_iter().next().expect("expected one payload");

    FlashblocksPayloadV1 {
        payload_id,
        index: 0,
        diff,
        metadata: Default::default(),
        base: Some(benchmark_base_payload()),
    }
}

/// Builds a [`FlashblocksPayloadV1`] fixture with the given number of transactions.
///
/// Returns the payload ready to be passed to `process_flashblock`.
/// The base references `CHAIN_SPEC.genesis_hash()` as `parent_hash` so
/// `BenchProvider` can serve the sealed header from `sealed_header_by_hash`.
pub fn build_flashblock_fixture<F>(
    num_txs: usize,
    bal: bool,
    mut build_tx_op: F,
) -> FlashblocksPayloadV1
where
    F: FnMut() -> TxOp,
{
    // Build a simple sequence of transactions from ALICE.
    let sequence: Vec<(TxOp, u64)> = (0..num_txs).map(|i| (build_tx_op(), i as u64)).collect();

    build_flashblock_fixture_from_sequence(sequence, bal, PayloadId::new([1u8; 8]))
}

pub fn build_flashblock_sequence_fixture_eth_transfers(
    num_flashblocks: usize,
    txs_per_flashblock: usize,
    bal: bool,
) -> Vec<FlashblocksPayloadV1> {
    build_flashblock_sequence_fixture(num_flashblocks, txs_per_flashblock, bal, || {
        TxOp::Transfer {
            from: ALICE.clone(),
            to: Address::random(),
            value: U256::from(100),
        }
    })
}

pub fn build_flashblock_sequence_fixture_fib(
    num_flashblocks: usize,
    txs_per_flashblock: usize,
    bal: bool,
) -> Vec<FlashblocksPayloadV1> {
    build_flashblock_sequence_fixture(num_flashblocks, txs_per_flashblock, bal, || TxOp::Fib {
        from: ALICE.clone(),
        n: 300,
        target: ChaosTarget::Proxy,
    })
}

pub fn build_flashblock_sequence_fixture_world_id_like_bn254(
    num_flashblocks: usize,
    txs_per_flashblock: usize,
    bal: bool,
) -> Vec<FlashblocksPayloadV1> {
    build_flashblock_sequence_fixture_from_sequence(
        build_world_id_bench_transaction_sequence(num_flashblocks * txs_per_flashblock),
        num_flashblocks,
        bal,
        PayloadId::new([4u8; 8]),
    )
}

fn build_flashblock_sequence_fixture_from_sequence(
    sequence: Vec<(TxOp, u64)>,
    num_flashblocks: usize,
    bal: bool,
    payload_id: PayloadId,
) -> Vec<FlashblocksPayloadV1> {
    let payloads = build_chained_payloads(sequence, num_flashblocks, bal)
        .expect("failed to build chained payloads");

    let base = benchmark_base_payload();

    payloads
        .into_iter()
        .enumerate()
        .map(|(i, (diff, _committed_state))| FlashblocksPayloadV1 {
            payload_id,
            index: i as u64,
            diff,
            metadata: Default::default(),
            base: if i == 0 { Some(base.clone()) } else { None },
        })
        .collect()
}

fn build_world_id_bench_transaction_sequence(total_txs: usize) -> Vec<(TxOp, u64)> {
    let mut sender_nonces = [0u64; 3];

    // Bias heavily toward one repeated proof/input pair while periodically injecting a second
    // valid proof shape to better approximate mainnet traffic and expose future cache wins.
    (0..total_txs)
        .map(|tx_index| {
            let sender_index = tx_index % 3;
            let from = match sender_index {
                0 => ALICE.clone(),
                1 => BOB.clone(),
                _ => CHARLIE.clone(),
            };
            let nonce = sender_nonces[sender_index];
            sender_nonces[sender_index] += 1;

            let case = if tx_index % WORLD_ID_ALT_INPUT_INTERVAL == WORLD_ID_ALT_INPUT_INTERVAL - 1
            {
                &WORLD_ID_BENCH_CASES[1]
            } else {
                &WORLD_ID_BENCH_CASES[0]
            };

            (
                TxOp::WorldIdVerifyCompressed {
                    from,
                    case: Box::new(*case),
                },
                nonce,
            )
        })
        .collect()
}

/// Builds a sequence of [`FlashblocksPayloadV1`] fixtures representing a
/// multi-flashblock epoch.
///
/// Returns `num_flashblocks` payloads sharing the same `payload_id`:
/// - Index 0 carries `base: Some(...)` (epoch start)
/// - Index 1+ carries `base: None` (continuation)
///
/// Each flashblock contains `txs_per_flashblock` transfer transactions.
/// The diffs are produced by `build_chained_payloads` so they represent
/// a valid incremental execution chain.
pub fn build_flashblock_sequence_fixture<F>(
    num_flashblocks: usize,
    txs_per_flashblock: usize,
    bal: bool,
    mut build_tx_op: F,
) -> Vec<FlashblocksPayloadV1>
where
    F: FnMut() -> TxOp,
{
    assert!(num_flashblocks > 0, "need at least 1 flashblock");

    let total_txs = num_flashblocks * txs_per_flashblock;

    // Build a sequence of transactions with correct per-sender nonces.
    let sequence: Vec<(TxOp, u64)> = (0..total_txs).map(|i| (build_tx_op(), i as u64)).collect();

    build_flashblock_sequence_fixture_from_sequence(
        sequence,
        num_flashblocks,
        bal,
        PayloadId::new([2u8; 8]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_world_id_like_flashblock_fixture() {
        let fixture = build_flashblock_fixture_world_id_like_bn254(3, false);
        assert_eq!(fixture.diff.transactions.len(), 3);
    }

    #[test]
    fn builds_world_id_like_flashblock_sequence_fixture() {
        let fixtures = build_flashblock_sequence_fixture_world_id_like_bn254(2, 3, false);
        assert_eq!(fixtures.len(), 2);
        assert_eq!(
            fixtures
                .iter()
                .map(|fixture| fixture.diff.transactions.len())
                .sum::<usize>(),
            6
        );
    }
}
