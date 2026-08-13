//! Karst L2 checks adapted from upstream OP Stack acceptance coverage:
//! https://github.com/ethereum-optimism/optimism/tree/develop/op-acceptance-tests/tests

use std::{borrow::Cow, time::Instant};

use alloy_consensus::TxReceipt;
use alloy_primitives::{Address, B256, Bytes, TxKind, U256, address};
use alloy_provider::{DynProvider, Provider, ProviderBuilder, RootProvider};
use alloy_rpc_types::{TransactionInput, TransactionRequest};
use alloy_rpc_types_eth::{Log, TransactionReceipt};
use alloy_sol_types::{SolCall, sol};
use eyre::eyre::{Context, OptionExt, bail, ensure};
use op_alloy_consensus::{OpTxType, TxDeposit, UserDepositSource};
use op_alloy_network::Optimism;
use op_alloy_rpc_types::OpTransactionReceipt;
use revm::bytecode::opcode;
use serde_json::Value;
use tokio::time::sleep;
use tracing::{info, warn};

use super::{
    rpc::RpcEnv,
    utils::{L2TxSender, wait_for_transaction_receipt},
};

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

sol! {
    contract OptimismPortal {
        function depositTransaction(address _to, uint256 _value, uint64 _gasLimit, bool _isCreation, bytes _data) external payable;
        event TransactionDeposited(address indexed from, address indexed to, uint256 indexed version, bytes opaqueData);
    }
}

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
        eprintln!("karst: ACCEPTANCE_KARST_ENABLED not set, skipping L2 execution checks");
        return Ok(());
    }
    let l2_key = env.config().l2_key.clone();

    eprintln!(
        "karst: running L2 execution checks: network={}, l2_key_address={}",
        env.config().network,
        l2_key.address()
    );
    info!(
        network = %env.config().network,
        l2_key_address = %l2_key.address(),
        "running Karst L2 execution acceptance tests"
    );

    eprintln!("karst: checking EIP-7910 eth_config");
    check_eip_7910_eth_config(env).await?;

    let mut sender = L2TxSender::new(
        env.chain_provider().clone(),
        l2_key,
        env.config().tx_timeout,
        env.config().tx_poll_interval,
    )
    .await?;

    eprintln!("karst: checking EIP-7823 MODEXP input bound");
    check_eip_7823_modexp_upper_bound(&mut sender).await?;
    eprintln!("karst: checking EIP-7883 MODEXP gas floor");
    check_eip_7883_modexp_gas_floor_increase(&mut sender).await?;
    eprintln!("karst: checking EIP-7951 P256VERIFY gas cost");
    check_eip_7951_p256verify_gas_cost(&mut sender).await?;
    eprintln!("karst: checking bn256 pairing input limit");
    check_karst_bn256_pairing_input_size_reduction(&mut sender).await?;
    eprintln!("karst: checking EIP-7939 CLZ opcode activation");
    check_eip_7939_clz_opcode_activation(&mut sender).await?;
    eprintln!("karst: checking EIP-7825 transaction gas cap");
    check_eip_7825_tx_gas_limit_cap(&mut sender).await?;
    eprintln!("karst: checking EIP-7825 deposit bypass");
    check_eip_7825_deposit_bypasses_tx_gas_limit_cap(env).await?;

    eprintln!("karst: L2 execution checks completed");
    info!("Karst L2 execution acceptance tests completed");
    Ok(())
}

async fn check_eip_7910_eth_config(env: &RpcEnv) -> eyre::Result<()> {
    let response: Value = env
        .chain_provider()
        .raw_request(Cow::Borrowed("eth_config"), ())
        .await
        .context("EIP-7910 eth_config RPC failed")?;

    ensure!(
        response.get("next").is_some(),
        "EIP-7910 eth_config response missing next fork field"
    );
    ensure!(
        response.get("last").is_some(),
        "EIP-7910 eth_config response missing last fork field"
    );

    let current = response
        .get("current")
        .and_then(Value::as_object)
        .ok_or_eyre("EIP-7910 eth_config response missing current fork config")?;

    let chain_id = current
        .get("chainId")
        .and_then(Value::as_str)
        .ok_or_eyre("EIP-7910 eth_config current config missing chainId")?;
    let expected_chain_id = format!("{:#x}", env.config().expected_chain_id);
    ensure!(
        chain_id.eq_ignore_ascii_case(&expected_chain_id),
        "EIP-7910 eth_config chainId mismatch: expected {expected_chain_id}, got {chain_id}"
    );
    ensure!(
        current.get("activationTime").is_some(),
        "EIP-7910 eth_config current config missing activationTime"
    );

    let fork_id = current
        .get("forkId")
        .and_then(Value::as_str)
        .ok_or_eyre("EIP-7910 eth_config current config missing forkId")?;
    let blob_schedule = current
        .get("blobSchedule")
        .and_then(Value::as_object)
        .ok_or_eyre("EIP-7910 eth_config current config missing blobSchedule")?;
    for field in ["target", "max", "baseFeeUpdateFraction"] {
        ensure!(
            blob_schedule.contains_key(field),
            "EIP-7910 eth_config blobSchedule missing {field}"
        );
    }

    let precompiles = current
        .get("precompiles")
        .and_then(Value::as_object)
        .ok_or_eyre("EIP-7910 eth_config current config missing precompiles")?;
    let p256verify = precompiles
        .get("P256VERIFY")
        .and_then(Value::as_str)
        .ok_or_eyre("EIP-7910 eth_config precompiles missing P256VERIFY")?;
    let expected_p256verify = P256VERIFY_PRECOMPILE.to_string();
    ensure!(
        p256verify.eq_ignore_ascii_case(&expected_p256verify),
        "EIP-7910 eth_config P256VERIFY mismatch: expected {expected_p256verify}, got {p256verify}"
    );

    info!(
        chain_id,
        fork_id, p256verify, "EIP-7910 eth_config returned expected Karst fork metadata"
    );
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

async fn check_eip_7825_deposit_bypasses_tx_gas_limit_cap(env: &RpcEnv) -> eyre::Result<()> {
    let Some(config) = env.config().karst_deposit.as_ref() else {
        info!(
            "ACCEPTANCE_KARST_DEPOSIT_ENABLED not set, skipping Karst deposit bypass acceptance test"
        );
        eprintln!(
            "karst: ACCEPTANCE_KARST_DEPOSIT_ENABLED not set, skipping EIP-7825 deposit bypass"
        );
        return Ok(());
    };
    let l1_provider = env
        .l1_provider()
        .ok_or_eyre("Karst deposit L1 provider missing")?
        .clone();
    let l1_key = config.l1_key.clone();
    let l1_sender = l1_key.address();
    let l1_provider = ProviderBuilder::new()
        .wallet(l1_key)
        .connect_provider(l1_provider)
        .erased();
    let request =
        deposit_transaction_request(l1_sender, config.optimism_portal, config.deposit_value);
    // Send-time fee fillers do not run during our explicit eth_estimateGas preflight.
    let request = with_estimated_l1_fees(&l1_provider, request).await?;
    eprintln!(
        "karst: preflighting EIP-7825 deposit bypass: l1_rpc={}, l1_sender={}, optimism_portal={}, deposit_gas_limit={}",
        config.l1_rpc_target(),
        l1_sender,
        config.optimism_portal,
        MAX_TX_GAS + 1
    );

    let estimated_gas = match l1_provider.estimate_gas(request.clone()).await {
        Ok(estimated_gas) if estimated_gas <= config.deposit_max_l1_gas => estimated_gas,
        Ok(estimated_gas) => {
            warn!(
                estimated_gas,
                max_l1_gas = config.deposit_max_l1_gas,
                "EIP-7825 deposit bypass preflight exceeded the configured L1 gas limit; skipping environment-dependent check"
            );
            eprintln!(
                "karst: skipping EIP-7825 deposit bypass: estimated_l1_gas={} exceeds max_l1_gas={}",
                estimated_gas, config.deposit_max_l1_gas
            );
            return Ok(());
        }
        Err(err) => return Err(err).context("EIP-7825 deposit bypass preflight failed"),
    };
    info!(
        l1_rpc = %config.l1_rpc_target(),
        l1_sender = %l1_sender,
        optimism_portal = %config.optimism_portal,
        estimated_gas,
        max_fee_per_gas = ?request.max_fee_per_gas,
        max_priority_fee_per_gas = ?request.max_priority_fee_per_gas,
        max_l1_gas = config.deposit_max_l1_gas,
        deposit_gas_limit = MAX_TX_GAS + 1,
        "EIP-7825 deposit bypass preflight succeeded"
    );
    let request = request.gas_limit(
        estimated_gas
            .saturating_add(100_000)
            .min(config.deposit_max_l1_gas),
    );

    eprintln!(
        "karst: submitting EIP-7825 deposit bypass L1 tx: estimated_l1_gas={}, max_l1_gas={}",
        estimated_gas, config.deposit_max_l1_gas
    );
    let pending_tx = l1_provider
        .send_transaction(request)
        .await
        .context("EIP-7825 deposit bypass: failed to submit L1 deposit transaction")?;
    let l1_hash = *pending_tx.tx_hash();
    eprintln!("karst: waiting for EIP-7825 deposit bypass L1 receipt: l1_tx={l1_hash:?}");
    let l1_receipt = wait_for_transaction_receipt(
        &l1_provider,
        "EIP-7825 deposit bypass L1 deposit",
        l1_hash,
        config.deposit_timeout,
        config.deposit_poll_interval,
    )
    .await?;
    ensure!(
        l1_receipt.status(),
        "EIP-7825 deposit bypass: L1 deposit tx {l1_hash:?} reverted in block {:?}",
        l1_receipt.block_number
    );
    eprintln!(
        "karst: EIP-7825 deposit bypass L1 receipt included: l1_tx={l1_hash:?}, l1_block={:?}",
        l1_receipt.block_number
    );

    let deposit_tx = deposit_tx_from_receipt(&l1_receipt, config.optimism_portal)?;
    ensure!(
        deposit_tx.gas_limit == MAX_TX_GAS + 1,
        "EIP-7825 deposit bypass: L2 deposit gas limit mismatch: expected {}, got {}",
        MAX_TX_GAS + 1,
        deposit_tx.gas_limit
    );
    let l2_hash = deposit_tx.tx_hash();
    eprintln!("karst: waiting for EIP-7825 deposit bypass L2 receipt: l2_tx={l2_hash:?}");
    let l2_receipt = wait_for_deposit_receipt(
        env.optimism_provider(),
        "EIP-7825 deposit bypass L2 deposit",
        l2_hash,
        config.deposit_timeout,
        config.deposit_poll_interval,
    )
    .await?;
    ensure!(
        l2_receipt.status,
        "EIP-7825 deposit bypass: L2 deposit tx {l2_hash:?} reverted in block {:?}",
        l2_receipt.block_number
    );

    eprintln!(
        "karst: EIP-7825 deposit bypass included successfully: l1_tx={l1_hash:?}, l1_block={:?}, l2_tx={l2_hash:?}, l2_block={}",
        l1_receipt.block_number, l2_receipt.block_number
    );
    info!(
        l1_tx = ?l1_hash,
        l1_block = ?l1_receipt.block_number,
        l2_tx = ?l2_hash,
        l2_block = l2_receipt.block_number,
        "EIP-7825 deposit bypass included successfully"
    );
    Ok(())
}

struct DepositReceipt {
    status: bool,
    block_number: u64,
}

async fn wait_for_deposit_receipt(
    provider: &RootProvider<Optimism>,
    label: &str,
    hash: B256,
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
) -> eyre::Result<DepositReceipt> {
    let deadline = Instant::now() + timeout;
    loop {
        let receipt = provider
            .get_transaction_receipt(hash)
            .await
            .with_context(|| format!("{label}: failed to fetch receipt for {hash:?}"))?;
        if let Some(receipt) = receipt {
            return parse_deposit_receipt(label, hash, receipt);
        }

        ensure!(
            Instant::now() < deadline,
            "{label}: timed out waiting for receipt for {hash:?}"
        );
        sleep(poll_interval).await;
    }
}

fn parse_deposit_receipt(
    label: &str,
    hash: B256,
    receipt: OpTransactionReceipt,
) -> eyre::Result<DepositReceipt> {
    let tx_type = receipt.inner.inner.receipt.tx_type();
    ensure!(
        tx_type == OpTxType::Deposit,
        "{label}: expected deposit receipt type 0x7e for {hash:?}, got {tx_type}"
    );
    let block_number = receipt
        .inner
        .block_number
        .ok_or_eyre(format!("{label}: receipt for {hash:?} missing blockNumber"))?;

    Ok(DepositReceipt {
        status: receipt.inner.inner.status(),
        block_number,
    })
}

async fn with_estimated_l1_fees(
    provider: &DynProvider,
    request: TransactionRequest,
) -> eyre::Result<TransactionRequest> {
    let fees = provider
        .estimate_eip1559_fees()
        .await
        .context("failed to estimate L1 fees for Karst deposit")?;

    Ok(request
        .max_fee_per_gas(fees.max_fee_per_gas)
        .max_priority_fee_per_gas(fees.max_priority_fee_per_gas))
}

fn deposit_transaction_request(
    from: Address,
    portal: Address,
    deposit_value: U256,
) -> TransactionRequest {
    let call = OptimismPortal::depositTransactionCall {
        _to: from,
        _value: deposit_value,
        _gasLimit: MAX_TX_GAS + 1,
        _isCreation: false,
        _data: Bytes::new(),
    };

    TransactionRequest::default()
        .from(from)
        .to(portal)
        .value(deposit_value)
        .input(TransactionInput::new(call.abi_encode().into()))
}

fn deposit_tx_from_receipt(
    receipt: &TransactionReceipt,
    portal: Address,
) -> eyre::Result<TxDeposit> {
    for log in receipt.logs() {
        if log.address() != portal {
            continue;
        }
        if let Some(tx) = deposit_tx_from_log(log)? {
            return Ok(tx);
        }
    }

    bail!(
        "EIP-7825 deposit bypass: no TransactionDeposited event found in L1 receipt {:?}",
        receipt.transaction_hash
    )
}

fn deposit_tx_from_log(log: &Log) -> eyre::Result<Option<TxDeposit>> {
    let Ok(decoded) = log.log_decode_validate::<OptimismPortal::TransactionDeposited>() else {
        return Ok(None);
    };
    let event = decoded.data();
    ensure!(
        event.version == U256::ZERO,
        "EIP-7825 deposit bypass: unsupported deposit event version {}",
        event.version
    );
    let opaque = event.opaqueData.as_ref();
    ensure!(
        opaque.len() >= 73,
        "EIP-7825 deposit bypass: deposit opaque data too short: {} bytes",
        opaque.len()
    );

    let mint = u128::try_from(U256::from_be_slice(&opaque[..32]))
        .context("EIP-7825 deposit bypass: deposit mint does not fit in u128")?;
    let value = U256::from_be_slice(&opaque[32..64]);
    let gas_limit = u64::from_be_bytes(opaque[64..72].try_into()?);
    let is_creation = match opaque[72] {
        0 => false,
        1 => true,
        value => bail!("EIP-7825 deposit bypass: invalid deposit creation flag {value}"),
    };
    let input = Bytes::copy_from_slice(&opaque[73..]);
    let to = if is_creation {
        ensure!(
            event.to == Address::ZERO,
            "EIP-7825 deposit bypass: create deposit target must be zero, got {}",
            event.to
        );
        TxKind::Create
    } else {
        TxKind::Call(event.to)
    };
    let block_hash = log
        .block_hash
        .ok_or_eyre("EIP-7825 deposit bypass: L1 deposit log missing block hash")?;
    let log_index = log
        .log_index
        .ok_or_eyre("EIP-7825 deposit bypass: L1 deposit log missing log index")?;

    Ok(Some(TxDeposit {
        source_hash: UserDepositSource::new(B256::from(block_hash), log_index).source_hash(),
        from: event.from,
        to,
        mint,
        value,
        gas_limit,
        is_system_transaction: false,
        input,
    }))
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
