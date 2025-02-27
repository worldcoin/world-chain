use std::sync::{LazyLock, OnceLock};

use alloy::sol;
use alloy_consensus::TxEnvelope;
use alloy_network::{EthereumWallet, NetworkWallet, TransactionBuilder};
use alloy_signer_local::{coins_bip39::English, MnemonicBuilder};
use alloy_sol_types::SolCall;
use eyre::eyre;
use op_alloy_network::Optimism;
use reth::revm::State;
use reth::rpc::types::TransactionRequest;
use reth_evm::Database;
use reth_optimism_node::OpEvm;
use reth_provider::ProviderError;
use tokio::runtime::Handle;
use WorldChainBlockRegistry::stampBlockCall;

use crate::inspector::PBHCallTracer;

static BUILDER_MNEMONIC: LazyLock<String> =
    LazyLock::new(|| std::env::var("BUILDER_MNEMONIC").expect("BUILDER_MNEMONIC env var not set"));

sol! {
    #[sol(rpc)]
    interface WorldChainBlockRegistry {
        function stampBlock();
    }
}

pub async fn stamp_block_tx<E>(
    db: &mut dyn revm_primitives::db::components::state::State<Error = E>,
) -> eyre::Result<TxEnvelope> {
    let signer = MnemonicBuilder::<English>::default()
        .phrase(BUILDER_MNEMONIC.to_string())
        .build()?;

    let wallet = EthereumWallet::from(signer);
    let address = NetworkWallet::<Optimism>::default_signer_address(&wallet);
    let runtime = Handle::current();

    futures::executor::block_on(async move {
        runtime
            .spawn(async move {
                println!("Sending stamp block tx");
            })
            .await
            .unwrap()
    });

    let x = Ok(TransactionRequest::default()
        .nonce(0)
        .gas_limit(100000)
        .max_priority_fee_per_gas(1000000000)
        .with_chain_id(1)
        .with_call(&stampBlockCall {})
        .build(&wallet)
        .await
        .unwrap());

    x
}
