use alloy_primitives::{Address, U256, utils::format_units};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_sol_types::sol;
use anyhow::{Context, Result, bail};
use url::Url;

pub const ETHEREUM_MAINNET_CHAIN_ID: u64 = 1;

sol! {
    #[sol(rpc)]
    interface ISuccinctVApp {
        event TransactionPending(uint64 indexed onchainTx, uint8 indexed variant, bytes data);

        function minDepositAmount() external view returns (uint256);
        function prove() external view returns (address);
        function permitAndDeposit(
            address from,
            uint256 amount,
            uint256 deadline,
            uint8 v,
            bytes32 r,
            bytes32 s
        ) external returns (uint64 receipt);
    }

    #[sol(rpc)]
    interface IProveToken {
        function balanceOf(address account) external view returns (uint256);
        function decimals() external view returns (uint8);
        function name() external view returns (string);
        function nonces(address owner) external view returns (uint256);
        function DOMAIN_SEPARATOR() external view returns (bytes32);
    }

    #[derive(Debug)]
    struct Permit {
        address owner;
        address spender;
        uint256 value;
        uint256 nonce;
        uint256 deadline;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SettlementConfig {
    pub vapp_address: Address,
    pub prove_token_address: Address,
    pub min_deposit_amount: U256,
    pub prove_decimals: u8,
}

/// Validates the settlement endpoint and discovers the VApp's mutable token configuration.
pub async fn load_settlement_config(
    rpc_url: &str,
    vapp_address: Address,
) -> Result<SettlementConfig> {
    if vapp_address.is_zero() {
        bail!("SUCCINCT_VAPP_ADDRESS must not be the zero address");
    }

    let rpc_url = Url::parse(rpc_url).context("invalid SP1 Network L1 RPC URL")?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let chain_id = provider
        .get_chain_id()
        .await
        .context("reading settlement RPC chain ID")?;
    if chain_id != ETHEREUM_MAINNET_CHAIN_ID {
        bail!(
            "Succinct settlement transactions are supported only on Ethereum mainnet (chain ID 1), but SP1_NETWORK_L1_RPC_URL reported chain ID {chain_id}"
        );
    }

    let vapp_code = provider
        .get_code_at(vapp_address)
        .await
        .context("reading SuccinctVApp bytecode")?;
    if vapp_code.is_empty() {
        bail!("SUCCINCT_VAPP_ADDRESS {vapp_address} has no bytecode on Ethereum mainnet");
    }

    let vapp = ISuccinctVApp::new(vapp_address, provider.clone());
    let prove_token_address = vapp
        .prove()
        .call()
        .await
        .context("reading SuccinctVApp.prove()")?;
    if prove_token_address.is_zero() {
        bail!("SuccinctVApp.prove() returned the zero address");
    }
    let prove_code = provider
        .get_code_at(prove_token_address)
        .await
        .context("reading PROVE token bytecode")?;
    if prove_code.is_empty() {
        bail!(
            "SuccinctVApp.prove() returned {prove_token_address}, which has no bytecode on Ethereum mainnet"
        );
    }

    let min_deposit_amount = vapp
        .minDepositAmount()
        .call()
        .await
        .context("reading SuccinctVApp.minDepositAmount()")?;
    if min_deposit_amount.is_zero() {
        bail!("SuccinctVApp.minDepositAmount() returned zero");
    }

    let prove_decimals = IProveToken::new(prove_token_address, provider)
        .decimals()
        .call()
        .await
        .context("reading PROVE token decimals()")?;

    Ok(SettlementConfig {
        vapp_address,
        prove_token_address,
        min_deposit_amount,
        prove_decimals,
    })
}

pub fn format_prove(amount: U256, decimals: u8) -> String {
    format_units(amount, decimals).unwrap_or_else(|_| amount.to_string())
}

pub fn prove_as_f64(amount: U256, decimals: u8) -> Result<f64> {
    format_units(amount, decimals)
        .context("formatting PROVE base units")?
        .parse::<f64>()
        .context("parsing formatted PROVE balance")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_prove_base_units() {
        assert_eq!(format_prove(U256::from(1_250_000_u64), 6), "1.250000");
    }
}
