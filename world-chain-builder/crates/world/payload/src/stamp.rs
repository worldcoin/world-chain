use std::sync::{LazyLock, OnceLock};

use alloy::sol;
use alloy_consensus::TxEnvelope;
use alloy_network::{EthereumWallet, NetworkWallet, TransactionBuilder};
use alloy_signer_local::{coins_bip39::English, MnemonicBuilder};
use alloy_sol_types::SolCall;
use eyre::eyre;
use futures::executor::block_on;
use op_alloy_consensus::OpTxEnvelope;
use op_alloy_network::Optimism;
use op_alloy_rpc_types::OpTransactionRequest;
use reth::rpc::types::TransactionRequest;
use reth_optimism_node::OpEvm;
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

pub fn stamp_block_tx<DB>(
    evm: &mut OpEvm<'_, &mut PBHCallTracer, &mut DB>,
) -> eyre::Result<(revm_primitives::Address, OpTxEnvelope)>
where
    DB: revm::Database + revm::DatabaseCommit,
    <DB as revm::Database>::Error: std::fmt::Debug + Send + Sync + derive_more::Error + 'static,
{
    let signer = MnemonicBuilder::<English>::default()
        .phrase(BUILDER_MNEMONIC.to_string())
        .index(1)
        .unwrap()
        .build()?;

    let wallet = EthereumWallet::from(signer);
    let address = NetworkWallet::<Optimism>::default_signer_address(&wallet);
    let db = evm.db_mut();
    let nonce = db.basic(address).unwrap().unwrap().nonce;

    let runtime = Handle::current();

    futures::executor::block_on(async move {
        runtime
            .spawn(async move {
                println!("Sending stamp block tx");
            })
            .await
            .unwrap()
    });

    // spawn a new os thread
    let tx = std::thread::spawn(move || {
        block_on(async {
            OpTransactionRequest::default()
                .nonce(nonce)
                .gas_limit(100000)
                .max_priority_fee_per_gas(100_000_000)
                .max_fee_per_gas(100_000_000)
                .with_chain_id(4801)
                .with_call(&stampBlockCall {})
                .build(&wallet)
                .await
                .unwrap()
        })
    })
    .join()
    .unwrap();

    Ok((address, tx))
}
