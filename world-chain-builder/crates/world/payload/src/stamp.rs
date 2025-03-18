use std::collections::HashSet;

use crate::builder::WorldChainPayloadBuilderCtx;
use alloy::sol;
use alloy_consensus::SignableTransaction;
use alloy_network::{TransactionBuilder, TxSignerSync};
use eyre::eyre::eyre;
use op_alloy_rpc_types::OpTransactionRequest;
use op_revm::OpContext;
use reth_evm::Evm;
use reth_optimism_node::OpEvm;
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::Recovered;
use reth_primitives_traits::SignedTransaction;
use revm::Inspector;
use semaphore_rs::Field;
use WorldChainBlockRegistry::spendNullifierHashesCall;

sol! {
    #[sol(rpc)]
    interface WorldChainBlockRegistry {
        function spendNullifierHashes(uint256[] calldata _nullifierHashes) external;
    }
}

pub const SPEND_NULLIFIER_HASHES_GAS: u64 = 100_000;

pub fn spend_nullifiers_tx<DB, I, Client>(
    ctx: &WorldChainPayloadBuilderCtx<Client>,
    evm: &mut OpEvm<DB, I>,
    nullifier_hashes: HashSet<Field>,
) -> eyre::Result<Recovered<OpTransactionSigned>>
where
    I: Inspector<OpContext<DB>>,
    DB: revm::Database + revm::DatabaseCommit,
    <DB as revm::Database>::Error: std::fmt::Debug + Send + Sync + derive_more::Error + 'static,
{
    let nonce = evm
        .db_mut()
        .basic(ctx.builder_private_key.address())?
        .unwrap_or_default()
        .nonce;
    let base_fee: u128 = evm.ctx().block.basefee.into();
    let registry = ctx.block_registry;
    let chain_id = evm.ctx().cfg.chain_id;

    let mut tx = OpTransactionRequest::default()
        .nonce(nonce)
        .gas_limit(SPEND_NULLIFIER_HASHES_GAS)
        .max_priority_fee_per_gas(base_fee)
        .max_fee_per_gas(base_fee)
        .with_chain_id(chain_id)
        .with_call(&spendNullifierHashesCall {
            _nullifierHashes: nullifier_hashes.into_iter().collect(),
        })
        .to(registry)
        .build_typed_tx()
        .map_err(|e| eyre!("{:?}", e))?;

    let signature = ctx.builder_private_key.sign_transaction_sync(&mut tx)?;
    let signed: OpTransactionSigned = tx.into_signed(signature).into();
    Ok(signed.into_recovered_unchecked()?)
}
