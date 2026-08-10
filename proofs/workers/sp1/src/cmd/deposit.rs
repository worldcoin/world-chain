use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_primitives::{Address, B256, U256, utils::parse_units};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer::{Signer, SignerSync};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::eip712_domain;
use anyhow::{Context, Result, bail};
use clap::Args;
use url::Url;
use world_chain_proof_sp1_host::network_prover::{NetworkCreditClient, SignerType};
use world_chain_proof_tx_signer::{TransactionSigner, build_transaction_signer};

use super::succinct::{
    ETHEREUM_MAINNET_CHAIN_ID, IProveToken, ISuccinctVApp, Permit, format_prove,
    load_settlement_config,
};

const PERMIT_VALIDITY: Duration = Duration::from_secs(60 * 60);
const CREDIT_POLL_INTERVAL: Duration = Duration::from_secs(10);
const CREDIT_REFLECTION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Args)]
#[command(
    group = clap::ArgGroup::new("transaction_signer")
        .required(true)
        .multiple(false)
        .args(&["sp1_private_key", "sp1_kms_key_id"])
)]
pub struct DepositArgs {
    /// Human-readable amount of PROVE to deposit (for example `1000` or `12.5`).
    #[arg(long)]
    amount: String,

    /// Ethereum mainnet RPC used to submit the deposit transaction.
    #[arg(long, env = "SP1_NETWORK_L1_RPC_URL")]
    sp1_network_l1_rpc_url: String,

    /// SuccinctVApp proxy address on Ethereum mainnet.
    #[arg(long, env = "SUCCINCT_VAPP_ADDRESS")]
    succinct_vapp_address: Address,

    /// Private key for both the PROVE holder and the corresponding SP1 Network account.
    #[arg(long, env = "SP1_PRIVATE_KEY")]
    sp1_private_key: Option<String>,

    /// AWS KMS key ID for both the PROVE holder and the corresponding SP1 Network account.
    #[arg(long, env = "SP1_KMS_KEY_ID", hide_env_values = true)]
    sp1_kms_key_id: Option<String>,
}

pub async fn deposit(args: DepositArgs) -> Result<()> {
    let settlement =
        load_settlement_config(&args.sp1_network_l1_rpc_url, args.succinct_vapp_address)
            .await
            .context("validating Succinct settlement configuration")?;

    let amount = parse_prove_amount(&args.amount, settlement.prove_decimals)?;
    if amount < settlement.min_deposit_amount {
        bail!(
            "deposit amount {} PROVE is below SuccinctVApp.minDepositAmount() of {} PROVE",
            format_prove(amount, settlement.prove_decimals),
            format_prove(settlement.min_deposit_amount, settlement.prove_decimals),
        );
    }
    let l1_rpc_url =
        Url::parse(&args.sp1_network_l1_rpc_url).context("invalid SP1 Network L1 RPC URL")?;
    let (signer_secret, signer_type) = match (&args.sp1_private_key, &args.sp1_kms_key_id) {
        (Some(private_key), None) => (private_key.as_str(), SignerType::Local),
        (None, Some(key_id)) if !key_id.trim().is_empty() => (key_id.as_str(), SignerType::AwsKms),
        _ => bail!("configure exactly one SP1 private key or AWS KMS key ID"),
    };
    let l1_signer = build_l1_signer(signer_secret, signer_type, &l1_rpc_url).await?;
    let signer_address = match &l1_signer {
        TransactionSigner::Local(signer) => signer.address(),
        TransactionSigner::Aws(signer) => signer.address(),
    };
    let credit_client = NetworkCreditClient::new(signer_secret, signer_type).await?;
    let credit_before = credit_client
        .get_balance()
        .await
        .context("reading SP1 Network credit balance before deposit")?;
    let provider = ProviderBuilder::new()
        .wallet(l1_signer.wallet())
        .connect_http(l1_rpc_url);
    let token = IProveToken::new(settlement.prove_token_address, provider.clone());
    let vapp = ISuccinctVApp::new(settlement.vapp_address, provider.clone());

    let token_balance = token
        .balanceOf(signer_address)
        .call()
        .await
        .context("reading signer PROVE balance")?;
    if token_balance < amount {
        bail!(
            "insufficient PROVE balance for {signer_address}: have {} PROVE, need {} PROVE",
            format_prove(token_balance, settlement.prove_decimals),
            format_prove(amount, settlement.prove_decimals),
        );
    }

    let eth_balance = provider
        .get_balance(signer_address)
        .await
        .context("reading signer ETH balance")?;
    if eth_balance.is_zero() {
        bail!("{signer_address} has no ETH to pay Ethereum mainnet transaction fees");
    }

    let nonce = token
        .nonces(signer_address)
        .call()
        .await
        .context("reading PROVE permit nonce")?;
    let domain_separator = token
        .DOMAIN_SEPARATOR()
        .call()
        .await
        .context("reading PROVE permit domain separator")?;
    let token_name = token
        .name()
        .call()
        .await
        .context("reading PROVE token name")?;
    let permit_domain = eip712_domain! {
        name: token_name,
        version: "1",
        chain_id: ETHEREUM_MAINNET_CHAIN_ID,
        verifying_contract: settlement.prove_token_address,
    };
    let expected_domain_separator = permit_domain.separator();
    if expected_domain_separator != domain_separator {
        bail!(
            "PROVE token EIP-2612 domain mismatch: computed {expected_domain_separator}, on-chain {domain_separator}"
        );
    }
    let deadline = permit_deadline()?;
    let permit = Permit {
        owner: signer_address,
        spender: settlement.vapp_address,
        value: amount,
        nonce,
        deadline,
    };
    let signature = match &l1_signer {
        TransactionSigner::Local(signer) => signer.sign_typed_data_sync(&permit, &permit_domain),
        TransactionSigner::Aws(signer) => signer.sign_typed_data(&permit, &permit_domain).await,
    }
    .context("signing PROVE permit")?
    .as_bytes();
    let r = B256::from_slice(&signature[..32]);
    let s = B256::from_slice(&signature[32..64]);
    let v = signature[64];

    println!(
        "Depositing {} PROVE from {signer_address} through SuccinctVApp {}...",
        format_prove(amount, settlement.prove_decimals),
        settlement.vapp_address,
    );
    let pending = vapp
        .permitAndDeposit(signer_address, amount, deadline, v, r, s)
        .send()
        .await
        .context("submitting permitAndDeposit transaction")?;
    let tx_hash = *pending.tx_hash();
    println!("Submitted deposit transaction {tx_hash}");

    let receipt = pending
        .get_receipt()
        .await
        .with_context(|| format!("waiting for deposit transaction {tx_hash}"))?;
    if !receipt.status() {
        bail!("deposit transaction {tx_hash} reverted on-chain");
    }

    let onchain_receipt = receipt
        .logs()
        .iter()
        .filter(|log| log.address() == settlement.vapp_address)
        .find_map(|log| {
            log.log_decode_validate::<ISuccinctVApp::TransactionPending>()
                .ok()
                .map(|decoded| decoded.inner.data.onchainTx)
        })
        .with_context(|| {
            format!("deposit transaction {tx_hash} succeeded but emitted no TransactionPending")
        })?;
    println!(
        "Deposit transaction {tx_hash} confirmed; Succinct on-chain receipt ID: {onchain_receipt}"
    );

    wait_for_credit_reflection(
        &credit_client,
        credit_before,
        settlement.prove_decimals,
        tx_hash,
        onchain_receipt,
    )
    .await
}

fn parse_prove_amount(input: &str, decimals: u8) -> Result<U256> {
    let input = input.trim();
    if input.is_empty() || input.starts_with('-') {
        bail!("--amount must be a positive PROVE amount");
    }
    let amount: U256 = parse_units(input, decimals)
        .with_context(|| format!("invalid PROVE amount `{input}`"))?
        .into();
    if amount.is_zero() {
        bail!("--amount must be greater than zero");
    }
    Ok(amount)
}

fn permit_deadline() -> Result<U256> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    let deadline = now
        .checked_add(PERMIT_VALIDITY)
        .context("permit deadline overflow")?;
    Ok(U256::from(deadline.as_secs()))
}

async fn wait_for_credit_reflection(
    client: &NetworkCreditClient,
    credit_before: U256,
    decimals: u8,
    tx_hash: B256,
    onchain_receipt: u64,
) -> Result<()> {
    let timeout_at = tokio::time::Instant::now() + CREDIT_REFLECTION_TIMEOUT;
    loop {
        match client.get_balance().await {
            Ok(balance) if balance > credit_before => {
                println!(
                    "Deposit reflected in SP1 Network credits: {} -> {} PROVE",
                    format_prove(credit_before, decimals),
                    format_prove(balance, decimals),
                );
                return Ok(());
            }
            Ok(balance) => {
                println!(
                    "Waiting for Succinct receipt {onchain_receipt} to be reflected in SP1 Network credits (current: {} PROVE)...",
                    format_prove(balance, decimals),
                );
            }
            Err(error) => {
                eprintln!("Failed to refresh SP1 Network credit balance: {error:#}");
            }
        }

        if tokio::time::Instant::now() >= timeout_at {
            bail!(
                "deposit transaction {tx_hash} succeeded with Succinct on-chain receipt ID {onchain_receipt}, but the SP1 Network credit balance did not increase within {} minutes; the deposit is still pending network processing",
                CREDIT_REFLECTION_TIMEOUT.as_secs() / 60,
            );
        }
        tokio::time::sleep(CREDIT_POLL_INTERVAL).await;
    }
}

async fn build_l1_signer(
    secret: &str,
    signer_type: SignerType,
    rpc_url: &Url,
) -> Result<TransactionSigner> {
    let signer = match signer_type {
        SignerType::Local => {
            let private_key = secret
                .parse::<PrivateKeySigner>()
                .context("invalid SP1 private key")?;
            build_transaction_signer(Some(private_key), None, rpc_url).await
        }
        SignerType::AwsKms => {
            build_transaction_signer(None, Some(secret.to_owned()), rpc_url).await
        }
    };

    signer.context("initializing L1 transaction signer")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_human_readable_prove_amount() {
        assert_eq!(
            parse_prove_amount("12.5", 6).unwrap(),
            U256::from(12_500_000_u64)
        );
    }

    #[test]
    fn rejects_non_positive_prove_amounts() {
        assert!(parse_prove_amount("0", 18).is_err());
        assert!(parse_prove_amount("-1", 18).is_err());
    }
}
