use reth::revm::Inspector;
use revm::context::{ContextTr, Transaction};
use revm::interpreter::{CallOutcome, Gas, InstructionResult, InterpreterResult};
use revm_primitives::{Address, Bytes};

pub const PBH_CALL_TRACER_ERROR: &str = "Invalid PBH caller";
/// Inspector that traces calls into the `PBHEntryPoint`.
///
/// This inspector checks if a transaction calls into the `PBHEntryPoint` from an address that is
/// neither the transaction origin nor the `PBHSignatureAggregator`. If such a call is detected,
/// the transaction is marked as invalid and stored in `invalid_txs`.
#[derive(Debug)]
pub struct PBHCallTracer {
    /// The address of the `PBHEntryPoint` contract.
    /// Calls to this contract are monitored to enforce caller restrictions.
    pub pbh_entry_point: Address,
    /// The address of the `PBHSignatureAggregator`.
    /// This address is allowed to make calls to the `PBHEntryPoint`.
    pub pbh_signature_aggregator: Address,
    /// Whether the tracer is active.
    pub active: bool,
}

impl PBHCallTracer {
    pub fn new(pbh_entry_point: Address, pbh_signature_aggregator: Address) -> Self {
        Self {
            pbh_entry_point,
            pbh_signature_aggregator,
            active: true,
        }
    }
}

impl<CTX: ContextTr> Inspector<CTX> for PBHCallTracer {
    fn call(
        &mut self,
        context: &mut CTX,
        inputs: &mut reth::revm::interpreter::CallInputs,
    ) -> Option<CallOutcome> {
        if !self.active {
            return None;
        }

        // Check if the target address is the `PBHEntryPoint`. If the caller is not the tx origin
        // or the `PBHSignatureAggregator`, mark the tx as invalid.
        if inputs.target_address == self.pbh_entry_point
            && inputs.caller != context.tx().caller()
            && inputs.caller != self.pbh_signature_aggregator
        {
            *context.error() = Err(revm::context_interface::context::ContextError::Custom(
                PBH_CALL_TRACER_ERROR.to_string(),
            ));

            let res = InterpreterResult::new(
                InstructionResult::InvalidEXTCALLTarget,
                Bytes::default(),
                Gas::new(0),
            );

            return Some(CallOutcome::new(res, 0..0));
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, sync::Arc};

    use alloy_consensus::SignableTransaction;
    use alloy_network::TxSigner;
    use alloy_signer::Signer;
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::{sol, SolCall};
    use op_alloy_consensus::OpTypedTransaction;
    use reth::{
        chainspec::ChainSpec,
        rpc::types::{TransactionInput, TransactionRequest},
    };
    use reth_evm::{ConfigureEvm, ConfigureEvmEnv, Evm, EvmEnv};
    use reth_optimism_chainspec::OpChainSpec;
    use reth_optimism_node::OpEvmConfig;
    use reth_optimism_primitives::OpTransactionSigned;
    use revm::{
        db::{CacheDB, EmptyDB},
        Database,
    };
    use revm_primitives::{
        AccountInfo, Address, Bytecode, Bytes, ExecutionResult, ResultAndState, U256,
    };

    use crate::inspector::PBH_CALL_TRACER_ERROR;

    use super::PBHCallTracer;

    sol! {
        #[sol(deployed_bytecode = "0x6080604052348015600e575f5ffd5b50600436106030575f3560e01c8063158ef93e1460345780636141dc45146053575b5f5ffd5b5f54603f9060ff1681565b604051901515815260200160405180910390f35b60815f80547fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff00166001179055565b00fea26469706673582212205e3a95972d3d3228de1dc5326e604186e39c3439403fd3dd49a9500e5bd9cdc264736f6c634300081c0033")]
        contract MockPbhEntryPoint {
            bool public initialized;

            function pbh() public {
                initialized = true;
            }
        }

        #[sol(deployed_bytecode = "0x6080604052600436106100f35760003560e01c80634d2301cc1161008a578063a8b0574e11610059578063a8b0574e1461025a578063bce38bd714610275578063c3077fa914610288578063ee82ac5e1461029b57600080fd5b80634d2301cc146101ec57806372425d9d1461022157806382ad56cb1461023457806386d516e81461024757600080fd5b80633408e470116100c65780633408e47014610191578063399542e9146101a45780633e64a696146101c657806342cbb15c146101d957600080fd5b80630f28c97d146100f8578063174dea711461011a578063252dba421461013a57806327e86d6e1461015b575b600080fd5b34801561010457600080fd5b50425b6040519081526020015b60405180910390f35b61012d610128366004610a85565b6102ba565b6040516101119190610bbe565b61014d610148366004610a85565b6104ef565b604051610111929190610bd8565b34801561016757600080fd5b50437fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff0140610107565b34801561019d57600080fd5b5046610107565b6101b76101b2366004610c60565b610690565b60405161011193929190610cba565b3480156101d257600080fd5b5048610107565b3480156101e557600080fd5b5043610107565b3480156101f857600080fd5b50610107610207366004610ce2565b73ffffffffffffffffffffffffffffffffffffffff163190565b34801561022d57600080fd5b5044610107565b61012d610242366004610a85565b6106ab565b34801561025357600080fd5b5045610107565b34801561026657600080fd5b50604051418152602001610111565b61012d610283366004610c60565b61085a565b6101b7610296366004610a85565b610a1a565b3480156102a757600080fd5b506101076102b6366004610d18565b4090565b60606000828067ffffffffffffffff8111156102d8576102d8610d31565b60405190808252806020026020018201604052801561031e57816020015b6040805180820190915260008152606060208201528152602001906001900390816102f65790505b5092503660005b8281101561047757600085828151811061034157610341610d60565b6020026020010151905087878381811061035d5761035d610d60565b905060200281019061036f9190610d8f565b6040810135958601959093506103886020850185610ce2565b73ffffffffffffffffffffffffffffffffffffffff16816103ac6060870187610dcd565b6040516103ba929190610e32565b60006040518083038185875af1925050503d80600081146103f7576040519150601f19603f3d011682016040523d82523d6000602084013e6103fc565b606091505b50602080850191909152901515808452908501351761046d577f08c379a000000000000000000000000000000000000000000000000000000000600052602060045260176024527f4d756c746963616c6c333a2063616c6c206661696c656400000000000000000060445260846000fd5b5050600101610325565b508234146104e6576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601a60248201527f4d756c746963616c6c333a2076616c7565206d69736d6174636800000000000060448201526064015b60405180910390fd5b50505092915050565b436060828067ffffffffffffffff81111561050c5761050c610d31565b60405190808252806020026020018201604052801561053f57816020015b606081526020019060019003908161052a5790505b5091503660005b8281101561068657600087878381811061056257610562610d60565b90506020028101906105749190610e42565b92506105836020840184610ce2565b73ffffffffffffffffffffffffffffffffffffffff166105a66020850185610dcd565b6040516105b4929190610e32565b6000604051808303816000865af19150503d80600081146105f1576040519150601f19603f3d011682016040523d82523d6000602084013e6105f6565b606091505b5086848151811061060957610609610d60565b602090810291909101015290508061067d576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601760248201527f4d756c746963616c6c333a2063616c6c206661696c656400000000000000000060448201526064016104dd565b50600101610546565b5050509250929050565b43804060606106a086868661085a565b905093509350939050565b6060818067ffffffffffffffff8111156106c7576106c7610d31565b60405190808252806020026020018201604052801561070d57816020015b6040805180820190915260008152606060208201528152602001906001900390816106e55790505b5091503660005b828110156104e657600084828151811061073057610730610d60565b6020026020010151905086868381811061074c5761074c610d60565b905060200281019061075e9190610e76565b925061076d6020840184610ce2565b73ffffffffffffffffffffffffffffffffffffffff166107906040850185610dcd565b60405161079e929190610e32565b6000604051808303816000865af19150503d80600081146107db576040519150601f19603f3d011682016040523d82523d6000602084013e6107e0565b606091505b506020808401919091529015158083529084013517610851577f08c379a000000000000000000000000000000000000000000000000000000000600052602060045260176024527f4d756c746963616c6c333a2063616c6c206661696c656400000000000000000060445260646000fd5b50600101610714565b6060818067ffffffffffffffff81111561087657610876610d31565b6040519080825280602002602001820160405280156108bc57816020015b6040805180820190915260008152606060208201528152602001906001900390816108945790505b5091503660005b82811015610a105760008482815181106108df576108df610d60565b602002602001015190508686838181106108fb576108fb610d60565b905060200281019061090d9190610e42565b925061091c6020840184610ce2565b73ffffffffffffffffffffffffffffffffffffffff1661093f6020850185610dcd565b60405161094d929190610e32565b6000604051808303816000865af19150503d806000811461098a576040519150601f19603f3d011682016040523d82523d6000602084013e61098f565b606091505b506020830152151581528715610a07578051610a07576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601760248201527f4d756c746963616c6c333a2063616c6c206661696c656400000000000000000060448201526064016104dd565b506001016108c3565b5050509392505050565b6000806060610a2b60018686610690565b919790965090945092505050565b60008083601f840112610a4b57600080fd5b50813567ffffffffffffffff811115610a6357600080fd5b6020830191508360208260051b8501011115610a7e57600080fd5b9250929050565b60008060208385031215610a9857600080fd5b823567ffffffffffffffff811115610aaf57600080fd5b610abb85828601610a39565b90969095509350505050565b6000815180845260005b81811015610aed57602081850181015186830182015201610ad1565b81811115610aff576000602083870101525b50601f017fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe0169290920160200192915050565b600082825180855260208086019550808260051b84010181860160005b84811015610bb1578583037fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe001895281518051151584528401516040858501819052610b9d81860183610ac7565b9a86019a9450505090830190600101610b4f565b5090979650505050505050565b602081526000610bd16020830184610b32565b9392505050565b600060408201848352602060408185015281855180845260608601915060608160051b870101935082870160005b82811015610c52577fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffa0888703018452610c40868351610ac7565b95509284019290840190600101610c06565b509398975050505050505050565b600080600060408486031215610c7557600080fd5b83358015158114610c8557600080fd5b9250602084013567ffffffffffffffff811115610ca157600080fd5b610cad86828701610a39565b9497909650939450505050565b838152826020820152606060408201526000610cd96060830184610b32565b95945050505050565b600060208284031215610cf457600080fd5b813573ffffffffffffffffffffffffffffffffffffffff81168114610bd157600080fd5b600060208284031215610d2a57600080fd5b5035919050565b7f4e487b7100000000000000000000000000000000000000000000000000000000600052604160045260246000fd5b7f4e487b7100000000000000000000000000000000000000000000000000000000600052603260045260246000fd5b600082357fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff81833603018112610dc357600080fd5b9190910192915050565b60008083357fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe1843603018112610e0257600080fd5b83018035915067ffffffffffffffff821115610e1d57600080fd5b602001915036819003821315610a7e57600080fd5b8183823760009101908152919050565b600082357fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffc1833603018112610dc357600080fd5b600082357fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffa1833603018112610dc357600080fdfea2646970667358221220bb2b5c71a328032f97c676ae39a1ec2148d3e5d6f73d95e9b17910152d61f16264736f6c634300080c0033")]
        contract Multicall3{
            struct Call {
                address target;
                bytes callData;
            }

            function aggregate(Call[] calldata calls) public payable returns (uint256 blockNumber, bytes[] memory returnData);
        }
    }

    fn deploy_contracts(db: &mut CacheDB<EmptyDB>) -> (Address, Address) {
        let mock_pbh_entry_point = Address::random();
        let info = AccountInfo {
            code: Some(Bytecode::new_raw(
                MockPbhEntryPoint::DEPLOYED_BYTECODE.clone(),
            )),
            ..Default::default()
        };
        db.insert_account_info(mock_pbh_entry_point, info);

        let multicall3 = Address::random();
        let info = AccountInfo {
            code: Some(Bytecode::new_raw(Multicall3::DEPLOYED_BYTECODE.clone())),
            ..Default::default()
        };
        db.insert_account_info(multicall3, info);

        (mock_pbh_entry_point, multicall3)
    }

    fn execute_with_pbh_tracer(
        db: &mut CacheDB<EmptyDB>,
        tx: OpTransactionSigned,
        signer: Address,
        pbh_tracer: &mut PBHCallTracer,
    ) -> Result<ResultAndState, revm_primitives::EVMError<Infallible>> {
        let info = AccountInfo {
            balance: U256::MAX,
            ..Default::default()
        };
        db.insert_account_info(signer, info);

        let chain_spec = Arc::new(OpChainSpec::new(ChainSpec::default()));
        let evm_config = OpEvmConfig::new(chain_spec);

        let mut evm = evm_config.evm_with_env_and_inspector(db, EvmEnv::default(), pbh_tracer);
        let tx_env = evm_config.tx_env(&tx, signer);

        evm.transact_commit(tx_env)
    }

    #[tokio::test]
    async fn test_tx_origin_is_caller() -> eyre::Result<()> {
        let mut db = CacheDB::default();
        let (mock_pbh_entry_point, _) = deploy_contracts(&mut db);

        let data = Bytes::from(MockPbhEntryPoint::pbhCall::SELECTOR);
        let signer = PrivateKeySigner::random().with_chain_id(Some(1));

        // Create a transaction to call into the mock pbh entrypoint
        let tx_request = TransactionRequest::default()
            .to(mock_pbh_entry_point)
            .input(TransactionInput::new(data))
            .nonce(0)
            .from(signer.address())
            .max_priority_fee_per_gas(1)
            .max_fee_per_gas(10)
            .gas_limit(250000)
            .build_consensus_tx()
            .expect("Could not build tx");

        let mut tx_1559 = tx_request
            .eip1559()
            .expect("Could not build EIP-1559 tx")
            .to_owned();

        let signature = signer.sign_transaction(&mut tx_1559).await?;
        let tx = OpTransactionSigned::new(
            OpTypedTransaction::Eip1559(tx_1559.clone()),
            signature,
            tx_1559.signature_hash(),
        );

        let slot = U256::from(0);
        let pre_state = db.storage(mock_pbh_entry_point, slot).unwrap();
        assert_eq!(pre_state, U256::from(0));

        let mut pbh_tracer = PBHCallTracer::new(mock_pbh_entry_point, Address::random());
        let result_and_state =
            execute_with_pbh_tracer(&mut db, tx, signer.address(), &mut pbh_tracer)?;

        assert!(matches!(
            result_and_state.result,
            ExecutionResult::Success { .. }
        ));

        let post_state = db.storage(mock_pbh_entry_point, slot).unwrap();
        assert_eq!(post_state, U256::from(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_signature_aggregator_is_caller() -> eyre::Result<()> {
        let mut db = CacheDB::default();
        let (mock_pbh_entry_point, multicall) = deploy_contracts(&mut db);

        let pbh_call = MockPbhEntryPoint::pbhCall {}.abi_encode();
        let multicall_data = Bytes::from(
            Multicall3::aggregateCall {
                calls: vec![Multicall3::Call {
                    target: mock_pbh_entry_point,
                    callData: pbh_call.into(),
                }],
            }
            .abi_encode(),
        );

        let signer = PrivateKeySigner::random().with_chain_id(Some(1));

        // Create a tx targeting into the multicall, which will call into the pbh entry point
        let tx_request = TransactionRequest::default()
            .to(multicall)
            .input(TransactionInput::new(multicall_data))
            .nonce(0)
            .from(signer.address())
            .max_priority_fee_per_gas(1)
            .max_fee_per_gas(10)
            .gas_limit(250000)
            .build_consensus_tx()
            .expect("Could not build tx");

        let mut tx_1559 = tx_request
            .eip1559()
            .expect("Could not build EIP-1559 tx")
            .to_owned();

        let signature = signer.sign_transaction(&mut tx_1559).await?;
        let tx = OpTransactionSigned::new(
            OpTypedTransaction::Eip1559(tx_1559.clone()),
            signature,
            tx_1559.signature_hash(),
        );

        let slot = U256::from(0);
        let pre_state = db.storage(mock_pbh_entry_point, slot).unwrap();
        assert_eq!(pre_state, U256::from(0));

        let mut pbh_tracer = PBHCallTracer::new(mock_pbh_entry_point, multicall);
        let result_and_state =
            execute_with_pbh_tracer(&mut db, tx, signer.address(), &mut pbh_tracer)?;

        assert!(matches!(
            result_and_state.result,
            ExecutionResult::Success { .. }
        ));

        let post_state = db.storage(mock_pbh_entry_point, slot).unwrap();
        assert_eq!(post_state, U256::from(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_caller() -> eyre::Result<()> {
        let mut db = CacheDB::default();
        let (mock_pbh_entry_point, multicall) = deploy_contracts(&mut db);

        let pbh_call = MockPbhEntryPoint::pbhCall {}.abi_encode();
        let multicall_data = Bytes::from(
            Multicall3::aggregateCall {
                calls: vec![Multicall3::Call {
                    target: mock_pbh_entry_point,
                    callData: pbh_call.into(),
                }],
            }
            .abi_encode(),
        );

        let signer = PrivateKeySigner::random().with_chain_id(Some(1));

        // Create a tx targeting into the multicall, which will call into the pbh entry point
        let tx_request = TransactionRequest::default()
            .to(multicall)
            .input(TransactionInput::new(multicall_data))
            .nonce(0)
            .from(signer.address())
            .max_priority_fee_per_gas(1)
            .max_fee_per_gas(10)
            .gas_limit(250000)
            .build_consensus_tx()
            .expect("Could not build tx");

        let mut tx_1559 = tx_request
            .eip1559()
            .expect("Could not build EIP-1559 tx")
            .to_owned();

        let signature = signer.sign_transaction(&mut tx_1559).await?;
        let tx = OpTransactionSigned::new(
            OpTypedTransaction::Eip1559(tx_1559.clone()),
            signature,
            tx_1559.signature_hash(),
        );

        let slot = U256::from(0);
        let pre_state = db.storage(mock_pbh_entry_point, slot).unwrap();
        assert_eq!(pre_state, U256::from(0));

        // Specify a random address as the signature aggregator, disallowing the multicall to call into the pbh entry point
        let mut pbh_tracer = PBHCallTracer::new(mock_pbh_entry_point, Address::random());
        let res = execute_with_pbh_tracer(&mut db, tx, signer.address(), &mut pbh_tracer);

        let expected_err =
            revm_primitives::EVMError::<Infallible>::Custom(PBH_CALL_TRACER_ERROR.to_string());

        assert_eq!(res.err().unwrap(), expected_err);

        let post_state = db.storage(mock_pbh_entry_point, slot).unwrap();
        assert_eq!(post_state, U256::from(0));

        Ok(())
    }
}
