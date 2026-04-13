//! Fork-based integration tests for `worldchain_simulateUnsignedUserOp`.
//!
//! These tests fork World Chain mainnet via an RPC URL and run simulations
//! against real deployed contracts (WLD, USDC.e, etc.).
//!
//! Run with:
//! ```sh
//! WORLD_CHAIN_RPC_URL=https://worldchain-mainnet.g.alchemy.com/v2/<KEY> \
//!   cargo test -p world-chain-rpc --test fork_simulate -- --ignored --nocapture
//! ```

use alloy_op_evm::OpEvmFactory;
use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{SolCall, sol};
use reth_evm::op_revm::{OpSpecId, OpTransaction};
use reth_evm::{Evm as RethEvm, EvmFactory};
use revm::context::result::{ExecutionResult, Output};
use revm::context::{BlockEnv, CfgEnv, TxEnv};
use revm::state::AccountInfo;
use revm_database::{AlloyDB, CacheDB, WrapDatabaseAsync};
use revm_primitives::TxKind;

use world_chain_rpc::simulate::{
    decode_revert_reason, new_simulation_inspector, parse_asset_changes, parse_exposure_changes,
};

// ─── Constants ───────────────────────────────────────────────────────────────

const WLD: Address = address!("2cFc85d8E48F8EAB294be644d9E25C3030863003");
const ENTRY_POINT: Address = address!("0000000071727De22E5E9d8BAf0edAc6f37da032");
const CHAIN_ID: u64 = 480;

// ─── ABI fragments ───────────────────────────────────────────────────────────

sol! {
    function transfer(address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
    function name() external view returns (string);
    function symbol() external view returns (string);
    function decimals() external view returns (uint8);
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn rpc_url() -> String {
    std::env::var("WORLD_CHAIN_RPC_URL").expect("WORLD_CHAIN_RPC_URL not set")
}

fn evm_env() -> reth_evm::EvmEnv<OpSpecId> {
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::ISTHMUS);
    cfg.chain_id = CHAIN_ID;
    reth_evm::EvmEnv::new(cfg, BlockEnv::default())
}

/// Create a forked CacheDB backed by an AlloyDB hitting the World Chain RPC.
/// Uses `RootProvider` directly (which implements Debug, satisfying revm bounds).
fn make_forked_db() -> CacheDB<WrapDatabaseAsync<AlloyDB<alloy::network::Ethereum, alloy::providers::RootProvider>>> {
    let provider = alloy::providers::RootProvider::new_http(rpc_url().parse().unwrap());
    let alloy_db = AlloyDB::new(provider, revm_database::BlockId::latest());
    let wrapped = WrapDatabaseAsync::new(alloy_db).expect("needs tokio runtime");
    CacheDB::new(wrapped)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify the fork works by calling WLD.name(), WLD.symbol(), WLD.decimals().
#[tokio::test]
#[ignore = "requires WORLD_CHAIN_RPC_URL"]
async fn test_fork_view_calls() {
    let mut db = make_forked_db();
    let env = evm_env();
    let mut evm = OpEvmFactory::default().create_evm(&mut db, env);

    // name()
    let res = RethEvm::transact(
        &mut evm,
        OpTransaction {
            base: TxEnv {
                caller: Address::ZERO,
                kind: TxKind::Call(WLD),
                data: nameCall {}.abi_encode().into(),
                gas_limit: 100_000,
                gas_price: 0,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();

    let name: String = match res.result {
        ExecutionResult::Success {
            output: Output::Call(d),
            ..
        } => nameCall::abi_decode_returns(&d).unwrap(),
        other => panic!("name() failed: {other:?}"),
    };
    assert_eq!(name, "Worldcoin");

    // symbol()
    let res = RethEvm::transact(
        &mut evm,
        OpTransaction {
            base: TxEnv {
                caller: Address::ZERO,
                kind: TxKind::Call(WLD),
                data: symbolCall {}.abi_encode().into(),
                gas_limit: 100_000,
                gas_price: 0,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();

    let sym: String = match res.result {
        ExecutionResult::Success {
            output: Output::Call(d),
            ..
        } => symbolCall::abi_decode_returns(&d).unwrap(),
        other => panic!("symbol() failed: {other:?}"),
    };
    assert_eq!(sym, "WLD");

    // decimals()
    let res = RethEvm::transact(
        &mut evm,
        OpTransaction {
            base: TxEnv {
                caller: Address::ZERO,
                kind: TxKind::Call(WLD),
                data: decimalsCall {}.abi_encode().into(),
                gas_limit: 100_000,
                gas_price: 0,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();

    let dec: u8 = match res.result {
        ExecutionResult::Success {
            output: Output::Call(d),
            ..
        } => decimalsCall::abi_decode_returns(&d).unwrap(),
        other => panic!("decimals() failed: {other:?}"),
    };
    assert_eq!(dec, 18);
}

/// ERC-20 Transfer log is parsed into the correct AssetChange.
#[tokio::test]
#[ignore = "requires WORLD_CHAIN_RPC_URL"]
async fn test_erc20_transfer_log_parsing() {
    let from = address!("00000000000000000000000000000000000000AA");
    let to = address!("00000000000000000000000000000000000000BB");
    let amount = U256::from(1_000_000u64);

    let log = alloy_primitives::Log::new(
        address!("79A02482A880bCE3B13e09Da970dC34db4CD24d1"),
        vec![
            alloy_primitives::b256!(
                "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
            ),
            alloy_primitives::B256::left_padding_from(from.as_slice()),
            alloy_primitives::B256::left_padding_from(to.as_slice()),
        ],
        amount.to_be_bytes_vec().into(),
    )
    .unwrap();

    let changes = parse_asset_changes(&[log]);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change_type, "ERC20");
    assert_eq!(changes[0].from, from);
    assert_eq!(changes[0].to, to);
    assert_eq!(changes[0].raw_amount, "1000000");
    assert!(changes[0].token_id.is_none());
}

/// ERC-721 Transfer (4 topics) is distinguished from ERC-20 (3 topics).
#[tokio::test]
#[ignore = "requires WORLD_CHAIN_RPC_URL"]
async fn test_erc721_transfer_log_parsing() {
    let contract = address!("0000000000000000000000000000000000000721");
    let from = Address::ZERO;
    let to = address!("00000000000000000000000000000000DeaDBeef");
    let token_id = U256::from(42u64);

    let log = alloy_primitives::Log::new(
        contract,
        vec![
            alloy_primitives::b256!(
                "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
            ),
            alloy_primitives::B256::left_padding_from(from.as_slice()),
            alloy_primitives::B256::left_padding_from(to.as_slice()),
            token_id.into(),
        ],
        Bytes::new(),
    )
    .unwrap();

    let changes = parse_asset_changes(&[log]);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change_type, "ERC721");
    assert_eq!(changes[0].raw_amount, "1");
    assert_eq!(changes[0].token_id.as_deref(), Some("42"));
}

/// ERC-1155 TransferSingle log is parsed correctly.
#[tokio::test]
#[ignore = "requires WORLD_CHAIN_RPC_URL"]
async fn test_erc1155_transfer_single_log_parsing() {
    let contract = address!("0000000000000000000000000000000000001155");
    let operator = address!("00000000000000000000000000000000000000AA");
    let from = address!("00000000000000000000000000000000000000BB");
    let to = address!("00000000000000000000000000000000000000CC");

    let mut data = Vec::with_capacity(64);
    data.extend_from_slice(&U256::from(7u64).to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(100u64).to_be_bytes::<32>());

    let log = alloy_primitives::Log::new(
        contract,
        vec![
            alloy_primitives::b256!(
                "c3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62"
            ),
            alloy_primitives::B256::left_padding_from(operator.as_slice()),
            alloy_primitives::B256::left_padding_from(from.as_slice()),
            alloy_primitives::B256::left_padding_from(to.as_slice()),
        ],
        data.into(),
    )
    .unwrap();

    let changes = parse_asset_changes(&[log]);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change_type, "ERC1155");
    assert_eq!(changes[0].from, from);
    assert_eq!(changes[0].to, to);
    assert_eq!(changes[0].raw_amount, "100");
    assert_eq!(changes[0].token_id.as_deref(), Some("7"));
}

/// ERC-20 Approval log is parsed into the correct ExposureChange.
#[tokio::test]
#[ignore = "requires WORLD_CHAIN_RPC_URL"]
async fn test_erc20_approval_log_parsing() {
    let owner = address!("00000000000000000000000000000000DeaDBeef");
    let spender = address!("000000000022D473030F116dDEE9F6B43aC78BA3");

    let log = alloy_primitives::Log::new(
        WLD,
        vec![
            alloy_primitives::b256!(
                "8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925"
            ),
            alloy_primitives::B256::left_padding_from(owner.as_slice()),
            alloy_primitives::B256::left_padding_from(spender.as_slice()),
        ],
        U256::MAX.to_be_bytes_vec().into(),
    )
    .unwrap();

    let changes = parse_exposure_changes(&[log]);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].owner, owner);
    assert_eq!(changes[0].spender, spender);
    assert_eq!(changes[0].raw_amount, U256::MAX.to_string());
    assert!(!changes[0].is_approved_for_all);
}
