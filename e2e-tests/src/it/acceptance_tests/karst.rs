//! Karst L2 checks adapted from upstream OP Stack acceptance coverage:
//! https://github.com/ethereum-optimism/optimism/tree/develop/op-acceptance-tests/tests

use alloy_primitives::{Address, Bytes, address};
use eyre::eyre::{Context, OptionExt, ensure};
use revm::bytecode::opcode;
use tracing::info;

use super::{rpc::RpcEnv, utils::L2TxSender};

const MODEXP_PRECOMPILE: Address = address!("0000000000000000000000000000000000000005");
const BN256_PAIRING_PRECOMPILE: Address = address!("0000000000000000000000000000000000000008");
const P256VERIFY_PRECOMPILE: Address = address!("0000000000000000000000000000000000000100");

const EIP_7883_BOUNDARY_GAS: u64 = 21_300;
const EIP_7951_BOUNDARY_GAS: u64 = 24_500;
const EIP_7823_OVERSIZED_GAS_LIMIT: u64 = 2_000_000;
const BN256_PAIR_ELEMENT_LEN: usize = 192;
const KARST_BN256_PAIR_MAX_INPUT_SIZE: usize = 300 * BN256_PAIR_ELEMENT_LEN;
const KARST_BN256_PAIR_PROBE_GAS_LIMIT: u64 = 12_000_000;
const MAX_TX_GAS: u64 = 16_777_216;

// EVM init code that computes CLZ(1) and returns the 32-byte result.
// CLZ(1) = 255 because 1 has 255 leading zero bits in a uint256.
const CLZ_INIT_CODE: &[u8] = &[
    opcode::PUSH1,
    1,
    opcode::CLZ,
    opcode::PUSH1,
    0,
    opcode::MSTORE,
    opcode::PUSH1,
    32,
    opcode::PUSH1,
    0,
    opcode::RETURN,
];

pub(super) async fn l2_execution_checks(env: &RpcEnv) -> eyre::Result<()> {
    if !env.config().karst_enabled {
        info!("ACCEPTANCE_KARST_ENABLED not set, skipping Karst L2 acceptance tests");
        return Ok(());
    }
    let l2_key = env
        .config()
        .l2_key
        .clone()
        .ok_or_eyre("ACCEPTANCE_L2_KEY is required when ACCEPTANCE_KARST_ENABLED=true")?;

    info!(
        network = %env.config().network,
        l2_key_address = %l2_key.address(),
        "running Karst L2 execution acceptance tests"
    );

    let mut sender = L2TxSender::new(
        env.chain_provider().clone(),
        l2_key,
        env.config().tx_timeout,
        env.config().tx_poll_interval,
    )
    .await?;

    check_eip_7823_modexp_upper_bound(&mut sender).await?;
    check_eip_7883_modexp_gas_floor_increase(&mut sender).await?;
    check_eip_7951_p256verify_gas_cost(&mut sender).await?;
    check_karst_bn256_pairing_input_size_reduction(&mut sender).await?;
    check_eip_7939_clz_opcode_activation(&mut sender).await?;
    check_eip_7825_tx_gas_limit_cap(&mut sender).await?;

    info!("Karst L2 execution acceptance tests completed");
    Ok(())
}

async fn check_eip_7823_modexp_upper_bound(sender: &mut L2TxSender) -> eyre::Result<()> {
    sender
        .expect_receipt_status(
            "EIP-7823 oversized MODEXP input",
            Some(MODEXP_PRECOMPILE),
            oversized_modexp_input(),
            EIP_7823_OVERSIZED_GAS_LIMIT,
            false,
        )
        .await?;

    sender
        .expect_receipt_status(
            "EIP-7823 within-limit MODEXP input",
            Some(MODEXP_PRECOMPILE),
            build_modexp_input(&[2], &[3], &[5]),
            200_000,
            true,
        )
        .await?;

    Ok(())
}

async fn check_eip_7883_modexp_gas_floor_increase(sender: &mut L2TxSender) -> eyre::Result<()> {
    sender
        .expect_receipt_status(
            "EIP-7883 under-gas MODEXP call",
            Some(MODEXP_PRECOMPILE),
            Bytes::new(),
            EIP_7883_BOUNDARY_GAS,
            false,
        )
        .await?;

    sender
        .expect_receipt_status(
            "EIP-7883 within-floor MODEXP call",
            Some(MODEXP_PRECOMPILE),
            Bytes::new(),
            21_600,
            true,
        )
        .await?;

    Ok(())
}

async fn check_eip_7951_p256verify_gas_cost(sender: &mut L2TxSender) -> eyre::Result<()> {
    sender
        .expect_receipt_status(
            "EIP-7951 under-gas P256VERIFY call",
            Some(P256VERIFY_PRECOMPILE),
            Bytes::new(),
            EIP_7951_BOUNDARY_GAS,
            false,
        )
        .await?;

    sender
        .expect_receipt_status(
            "EIP-7951 within-cost P256VERIFY call",
            Some(P256VERIFY_PRECOMPILE),
            Bytes::new(),
            28_000,
            true,
        )
        .await?;

    Ok(())
}

async fn check_karst_bn256_pairing_input_size_reduction(
    sender: &mut L2TxSender,
) -> eyre::Result<()> {
    sender
        .expect_receipt_status(
            "Karst over-limit bn256 pairing input",
            Some(BN256_PAIRING_PRECOMPILE),
            Bytes::from(vec![
                0;
                KARST_BN256_PAIR_MAX_INPUT_SIZE + BN256_PAIR_ELEMENT_LEN
            ]),
            KARST_BN256_PAIR_PROBE_GAS_LIMIT,
            false,
        )
        .await?;

    sender
        .expect_receipt_status(
            "Karst within-limit bn256 pairing input",
            Some(BN256_PAIRING_PRECOMPILE),
            Bytes::from(vec![0; KARST_BN256_PAIR_MAX_INPUT_SIZE]),
            KARST_BN256_PAIR_PROBE_GAS_LIMIT,
            true,
        )
        .await?;

    Ok(())
}

async fn check_eip_7939_clz_opcode_activation(sender: &mut L2TxSender) -> eyre::Result<()> {
    let receipt = sender
        .expect_receipt_status(
            "EIP-7939 CLZ deployment",
            None,
            Bytes::copy_from_slice(CLZ_INIT_CODE),
            100_000,
            true,
        )
        .await?;

    let contract = receipt
        .contract_address
        .ok_or_eyre("EIP-7939 CLZ deployment receipt did not include a contract address")?;
    let block_hash = receipt
        .block_hash
        .ok_or_eyre("EIP-7939 CLZ deployment receipt did not include a block hash")?;
    let code = sender
        .code_at_block("EIP-7939 CLZ deployed code lookup", contract, block_hash)
        .await
        .wrap_err("failed to fetch deployed CLZ result code")?;

    let mut expected = [0_u8; 32];
    expected[31] = 0xff;
    ensure!(
        code.as_ref() == expected,
        "EIP-7939 CLZ deployed code mismatch: expected 0x{}, got {code:?}",
        alloy_primitives::hex::encode(expected)
    );

    Ok(())
}

async fn check_eip_7825_tx_gas_limit_cap(sender: &mut L2TxSender) -> eyre::Result<()> {
    sender
        .expect_submission_rejected(
            "EIP-7825 tx gas limit cap",
            Some(Address::ZERO),
            Bytes::new(),
            MAX_TX_GAS + 1,
        )
        .await
}

fn oversized_modexp_input() -> Bytes {
    const OVERSIZED_MOD_SIZE: usize = 1025;
    let mut modulus = vec![0; OVERSIZED_MOD_SIZE];
    modulus[OVERSIZED_MOD_SIZE - 1] = 5;
    build_modexp_input(&[2], &[3], &modulus)
}

fn build_modexp_input(base: &[u8], exp: &[u8], modulus: &[u8]) -> Bytes {
    let mut input = Vec::with_capacity(96 + base.len() + exp.len() + modulus.len());
    append_modexp_len(&mut input, base.len());
    append_modexp_len(&mut input, exp.len());
    append_modexp_len(&mut input, modulus.len());
    input.extend_from_slice(base);
    input.extend_from_slice(exp);
    input.extend_from_slice(modulus);
    input.into()
}

fn append_modexp_len(input: &mut Vec<u8>, len: usize) {
    let mut word = [0_u8; 32];
    word[24..].copy_from_slice(&(len as u64).to_be_bytes());
    input.extend_from_slice(&word);
}
