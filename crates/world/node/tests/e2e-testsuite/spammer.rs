use std::{sync::Arc, time::Duration};

use alloy_consensus::Header;
use alloy_eips::Encodable2718;
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_provider::ProviderBuilder;
use alloy_rpc_types::TransactionRequest;
use alloy_sol_types::{SolCall, sol};
use futures::{StreamExt, stream::FuturesOrdered};
use reqwest::Url;
use reth::{chainspec::EthChainSpec, rpc::api::EthApiClient};
use reth_e2e_test_utils::testsuite::NodeClient;
use reth_optimism_node::OpEngineTypes;
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use revm_primitives::{Address, B256, Bytes};
use tracing::{error, info};
use world_chain_test::{node::tx, utils::signer};

use crate::{actions::BlockProductionState, setup::CHAIN_SPEC};

sol! {
    #[sol(rpc, bytecode = "6080604052348015600e575f5ffd5b506101338061001c5f395ff3fe608060405234801561000f575f5ffd5b506004361061004a575f3560e01c80632b68b9c61461004e578063703c2d1a14610056578063affed0e01461005e578063b8dda9c71461007a575b5f5ffd5b61005433ff5b005b6100546100ac565b61006760015481565b6040519081526020015b60405180910390f35b61009c6100883660046100f7565b5f6020819052908152604090205460ff1681565b6040519015158152602001610071565b5f5b60648110156100f4576001805f8282546100c8919061010e565b9091555050600180545f908152602081905260409020805460ff19811660ff90911615179055016100ae565b50565b5f60208284031215610107575f5ffd5b5035919050565b8082018082111561012d57634e487b7160e01b5f52601160045260245ffd5b9291505056")]
    contract TestContract {
        mapping(uint256 => bool) public map;
        uint256 public nonce;

        constructor() {}

        function sstore() external {
            for (uint256 i = 0; i < 100; i++) {
                nonce += 1;
                bool value = map[nonce];
                map[nonce] = !value;
            }
        }

        function destruct() external {
            selfdestruct(payable(msg.sender));
        }
    }

    #[sol(rpc, bytecode = "6080604052348015600e575f5ffd5b506102f58061001c5f395ff3fe608060405234801561000f575f5ffd5b506004361061003f575f3560e01c80633fa4f2451461004357806366e30ada1461005d578063775c300c14610067575b5f5ffd5b61004b5f5481565b60405190815260200160405180910390f35b61006561006f565b005b610065610137565b5f60405161007c90610198565b604051809103905ff080158015610095573d5f5f3e3d5ffd5b509050806001600160a01b031663703c2d1a6040518163ffffffff1660e01b81526004015f604051808303815f87803b1580156100d0575f5ffd5b505af11580156100e2573d5f5f3e3d5ffd5b50505050806001600160a01b0316632b68b9c66040518163ffffffff1660e01b81526004015f604051808303815f87803b15801561011e575f5ffd5b505af1158015610130573d5f5f3e3d5ffd5b5050505050565b5f60405161014490610198565b604051809103905ff08015801561015d573d5f5f3e3d5ffd5b509050806001600160a01b031663703c2d1a6040518163ffffffff1660e01b81526004015f604051808303815f87803b15801561011e575f5ffd5b61014f806101a68339019056fe6080604052348015600e575f5ffd5b506101338061001c5f395ff3fe608060405234801561000f575f5ffd5b506004361061004a575f3560e01c80632b68b9c61461004e578063703c2d1a14610056578063affed0e01461005e578063b8dda9c71461007a575b5f5ffd5b61005433ff5b005b6100546100ac565b61006760015481565b6040519081526020015b60405180910390f35b61009c6100883660046100f7565b5f6020819052908152604090205460ff1681565b6040519015158152602001610071565b5f5b60648110156100f4576001805f8282546100c8919061010e565b9091555050600180545f908152602081905260409020805460ff19811660ff90911615179055016100ae565b50565b5f60208284031215610107575f5ffd5b5035919050565b8082018082111561012d57634e487b7160e01b5f52601160045260245ffd5b9291505056")]
    contract TestContractFactory {
        uint256 public value;

        constructor() {}

        function deploy() external {
            TestContract newContract = new TestContract();
            newContract.sstore();
        }

        function deployAndDestruct() external {
            TestContract newContract = new TestContract();
            newContract.sstore();
            newContract.destruct();
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TxType {
    Sstore,
    Deploy,
    DeployAndDestruct,
}

impl TxType {
    pub async fn to_raw(
        &self,
        signer_id: u32,
        nonce: u64,
        contract: Address,
        factory: Address,
    ) -> Bytes {
        let wallet = EthereumWallet::from(signer(signer_id));
        let (to, calldata) = match self {
            TxType::Sstore => (contract, TestContract::sstoreCall::SELECTOR.into()),
            TxType::Deploy => (factory, TestContractFactory::deployCall::SELECTOR.into()),
            TxType::DeployAndDestruct => (
                factory,
                TestContractFactory::deployAndDestructCall::SELECTOR.into(),
            ),
        };

        let tx = tx(CHAIN_SPEC.chain_id(), Some(calldata), nonce, to, 100_000);
        let signed = <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx, &wallet)
            .await
            .unwrap();

        signed.encoded_2718().into()
    }
}

/// Maximum number of unique signers available
const MAX_SIGNERS: u32 = 19;

#[derive(Debug)]
pub struct TxSpammer {
    pub rpc: Vec<NodeClient<OpEngineTypes>>,
    pub sequence: Vec<TxType>,
}

impl TxSpammer {
    /// Spawns a background task that continuously sends transactions.
    ///
    /// Deploys test contracts on startup, then sends batches of `tpf` transactions per flashblock.
    /// Each signer maintains its own nonce, automatically incremented after each use.
    pub fn spawn(self, tpf: u64, http_url: Url) {
        tokio::spawn(async move {
            // Deploy test contracts
            let wallet = EthereumWallet::from(signer(0));
            let provider = Arc::new(ProviderBuilder::new().wallet(wallet).connect_http(http_url));

            let contract = *TestContract::deploy(provider.clone())
                .await
                .unwrap()
                .address();

            let factory = *TestContractFactory::deploy(provider)
                .await
                .unwrap()
                .address();

            info!("Deployed TestContract at {contract}, TestContractFactory at {factory}");

            // Track nonce per signer (signers 1..=MAX_SIGNERS)
            let mut nonces: Vec<u64> = vec![0; MAX_SIGNERS as usize];

            loop {
                let batch = self.build_batch(tpf, &mut nonces, contract, factory).await;
                self.broadcast_batch(&batch).await;

                info!("Submitted {} transactions", batch.len());
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    /// Spawns a background task that sends transactions and reports hashes to shared state.
    ///
    /// Same as `spawn` but also records transaction hashes in the provided `BlockProductionState`
    /// for receipt querying by other actions.
    pub fn spawn_with_state(self, tpf: u64, http_url: Url, state: BlockProductionState) {
        tokio::spawn(async move {
            // Deploy test contracts
            let wallet = EthereumWallet::from(signer(0));
            let provider = Arc::new(ProviderBuilder::new().wallet(wallet).connect_http(http_url));

            let contract = *TestContract::deploy(provider.clone())
                .await
                .unwrap()
                .address();

            let factory = *TestContractFactory::deploy(provider)
                .await
                .unwrap()
                .address();

            info!("Deployed TestContract at {contract}, TestContractFactory at {factory}");

            // Track nonce per signer (signers 1..=MAX_SIGNERS)
            let mut nonces: Vec<u64> = vec![0; MAX_SIGNERS as usize];

            loop {
                let batch = self.build_batch(tpf, &mut nonces, contract, factory).await;
                let tx_hashes = self.broadcast_batch_with_hashes(&batch).await;

                // Record submitted tx hashes in shared state
                for hash in tx_hashes {
                    state.add_tx_hash(hash);
                }

                info!("Submitted {} transactions", batch.len());
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    /// Builds a batch of `tpf` raw transactions, distributing them across signers round-robin.
    async fn build_batch(
        &self,
        tpf: u64,
        nonces: &mut [u64],
        contract: Address,
        factory: Address,
    ) -> Vec<Bytes> {
        let mut futures = Vec::with_capacity(tpf as usize);

        for i in 0..tpf {
            // Round-robin signer selection (signers are 1-indexed)
            let signer_idx = (i as usize) % nonces.len();
            let signer_id = (signer_idx + 1) as u32;
            let nonce = nonces[signer_idx];

            // Increment nonce for next use
            nonces[signer_idx] += 1;

            // Select transaction type from sequence
            let tx_type = self.sequence[i as usize % self.sequence.len()]
                .to_raw(signer_id, nonce, contract, factory);

            futures.push(tx_type);
        }

        futures::future::join_all(futures).await
    }

    /// Broadcasts a batch of transactions to all RPC clients.
    async fn broadcast_batch(&self, batch: &[Bytes]) {
        let mut futs = FuturesOrdered::new();
        for client in &self.rpc {
            for tx in batch {
                futs.push_back(async move {
                    let result =
                        EthApiClient::<
                            TransactionRequest,
                            OpTransactionSigned,
                            alloy_consensus::Block<OpTransactionSigned>,
                            OpReceipt,
                            Header,
                            Bytes,
                        >::send_raw_transaction(&client.rpc, tx.clone())
                        .await
                        .inspect_err(|e| error!("Error sending transaction: {:?}", e));

                    match result {
                        Ok(tx_hash) => info!("Submitted tx: {:?}", tx_hash),
                        Err(_) => {} // already logged
                    }
                });
            }
        }

        futs.collect::<()>().await;
    }

    /// Broadcasts a batch of transactions and returns the successful tx hashes.
    async fn broadcast_batch_with_hashes(&self, batch: &[Bytes]) -> Vec<B256> {
        let mut tx_hashes = Vec::with_capacity(batch.len());

        for client in &self.rpc {
            for tx in batch {
                let result = EthApiClient::<
                    TransactionRequest,
                    OpTransactionSigned,
                    alloy_consensus::Block<OpTransactionSigned>,
                    OpReceipt,
                    Header,
                    Bytes,
                >::send_raw_transaction(&client.rpc, tx.clone())
                .await
                .inspect_err(|e| error!("Error sending transaction: {:?}", e));

                if let Ok(tx_hash) = result {
                    info!("Submitted tx: {:?}", tx_hash);
                    tx_hashes.push(tx_hash);
                }
            }
            // Only send to first client to avoid duplicates
            break;
        }

        tx_hashes
    }
}
