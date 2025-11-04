use std::collections::HashMap;

use alloy_eips::{eip2935::HISTORY_STORAGE_ADDRESS, eip4788::BEACON_ROOTS_ADDRESS};
use alloy_primitives::{address, b256, hex, Address, Bytes, B256};
use reth::revm::State;
use reth_chainspec::EthereumHardforks;
use reth_evm::{
    block::{BlockExecutionError, BlockValidationError},
    Evm,
};
use reth_optimism_forks::OpHardforks;
use revm::{
    context::result::ResultAndState,
    state::{Account, AccountInfo, Bytecode},
    Database, DatabaseCommit,
};

/// The address of the create2 deployer
pub(crate) const CREATE_2_DEPLOYER_ADDR: Address =
    address!("0x13b0D85CcB8bf860b6b79AF3029fCA081AE9beF2");

/// The codehash of the create2 deployer contract.
pub(crate) const CREATE_2_DEPLOYER_CODEHASH: B256 =
    b256!("0xb0550b5b431e30d38000efb7107aaa0ade03d48a7198a140edda9d27134468b2");

/// The raw bytecode of the create2 deployer contract.
pub(crate) const CREATE_2_DEPLOYER_BYTECODE: [u8; 1584] = hex!("6080604052600436106100435760003560e01c8063076c37b21461004f578063481286e61461007157806356299481146100ba57806366cfa057146100da57600080fd5b3661004a57005b600080fd5b34801561005b57600080fd5b5061006f61006a366004610327565b6100fa565b005b34801561007d57600080fd5b5061009161008c366004610327565b61014a565b60405173ffffffffffffffffffffffffffffffffffffffff909116815260200160405180910390f35b3480156100c657600080fd5b506100916100d5366004610349565b61015d565b3480156100e657600080fd5b5061006f6100f53660046103ca565b610172565b61014582826040518060200161010f9061031a565b7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe082820381018352601f90910116604052610183565b505050565b600061015683836102e7565b9392505050565b600061016a8484846102f0565b949350505050565b61017d838383610183565b50505050565b6000834710156101f4576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601d60248201527f437265617465323a20696e73756666696369656e742062616c616e636500000060448201526064015b60405180910390fd5b815160000361025f576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820181905260248201527f437265617465323a2062797465636f6465206c656e677468206973207a65726f60448201526064016101eb565b8282516020840186f5905073ffffffffffffffffffffffffffffffffffffffff8116610156576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601960248201527f437265617465323a204661696c6564206f6e206465706c6f790000000000000060448201526064016101eb565b60006101568383305b6000604051836040820152846020820152828152600b8101905060ff815360559020949350505050565b61014e806104ad83390190565b6000806040838503121561033a57600080fd5b50508035926020909101359150565b60008060006060848603121561035e57600080fd5b8335925060208401359150604084013573ffffffffffffffffffffffffffffffffffffffff8116811461039057600080fd5b809150509250925092565b7f4e487b7100000000000000000000000000000000000000000000000000000000600052604160045260246000fd5b6000806000606084860312156103df57600080fd5b8335925060208401359150604084013567ffffffffffffffff8082111561040557600080fd5b818601915086601f83011261041957600080fd5b81358181111561042b5761042b61039b565b604051601f82017fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe0908116603f011681019083821181831017156104715761047161039b565b8160405282815289602084870101111561048a57600080fd5b826020860160208301376000602084830101528095505050505050925092509256fe608060405234801561001057600080fd5b5061012e806100206000396000f3fe6080604052348015600f57600080fd5b506004361060285760003560e01c8063249cb3fa14602d575b600080fd5b603c603836600460b1565b604e565b60405190815260200160405180910390f35b60008281526020818152604080832073ffffffffffffffffffffffffffffffffffffffff8516845290915281205460ff16608857600060aa565b7fa2ef4600d742022d532d4747cb3547474667d6f13804902513b2ec01c848f4b45b9392505050565b6000806040838503121560c357600080fd5b82359150602083013573ffffffffffffffffffffffffffffffffffffffff8116811460ed57600080fd5b80915050925092905056fea26469706673582212205ffd4e6cede7d06a5daf93d48d0541fc68189eeb16608c1999a82063b666eb1164736f6c63430008130033a2646970667358221220fdc4a0fe96e3b21c108ca155438d37c9143fb01278a3c1d274948bad89c564ba64736f6c63430008130033");

/// Applies the pre-block call to the [EIP-4788] beacon block root contract, using the given block,
/// chain spec, EVM.
///
/// Note: this does not commit the state changes to the database, it only transact the call.
///
/// Returns `None` if Cancun is not active or the block is the genesis block, otherwise returns the
/// result of the call.
///
/// [EIP-4788]: https://eips.ethereum.org/EIPS/eip-4788
#[inline]
pub(crate) fn transact_beacon_root_contract_call<Halt>(
    spec: impl EthereumHardforks,
    parent_beacon_block_root: Option<B256>,
    evm: &mut impl Evm<HaltReason = Halt>,
) -> Result<Option<ResultAndState<Halt>>, BlockExecutionError> {
    if !spec.is_cancun_active_at_timestamp(evm.block().timestamp.saturating_to()) {
        return Ok(None);
    }

    let parent_beacon_block_root =
        parent_beacon_block_root.ok_or(BlockValidationError::MissingParentBeaconBlockRoot)?;

    // if the block number is zero (genesis block) then the parent beacon block root must
    // be 0x0 and no system transaction may occur as per EIP-4788
    if evm.block().number.is_zero() {
        if !parent_beacon_block_root.is_zero() {
            return Err(
                BlockValidationError::CancunGenesisParentBeaconBlockRootNotZero {
                    parent_beacon_block_root,
                }
                .into(),
            );
        }
        return Ok(None);
    }

    let res = match evm.transact_system_call(
        alloy_eips::eip4788::SYSTEM_ADDRESS,
        BEACON_ROOTS_ADDRESS,
        parent_beacon_block_root.0.into(),
    ) {
        Ok(res) => res,
        Err(e) => {
            return Err(BlockValidationError::BeaconRootContractCall {
                parent_beacon_block_root: Box::new(parent_beacon_block_root),
                message: e.to_string(),
            }
            .into())
        }
    };

    Ok(Some(res))
}

/// Applies the pre-block call to the [EIP-2935] blockhashes contract, using the given block,
/// chain specification, and EVM.
///
/// If Prague is not activated, or the block is the genesis block, then this is a no-op, and no
/// state changes are made.
///
/// Note: this does not commit the state changes to the database, it only transact the call.
///
/// Returns `None` if Prague is not active or the block is the genesis block, otherwise returns the
/// result of the call.
///
/// [EIP-2935]: https://eips.ethereum.org/EIPS/eip-2935
#[inline]
pub(crate) fn transact_blockhashes_contract_call<Halt>(
    spec: impl EthereumHardforks,
    parent_block_hash: B256,
    evm: &mut impl Evm<HaltReason = Halt>,
) -> Result<Option<ResultAndState<Halt>>, BlockExecutionError> {
    if !spec.is_prague_active_at_timestamp(evm.block().timestamp.saturating_to()) {
        return Ok(None);
    }

    // if the block number is zero (genesis block) then no system transaction may occur as per
    // EIP-2935
    if evm.block().number.is_zero() {
        return Ok(None);
    }

    let res = match evm.transact_system_call(
        alloy_eips::eip4788::SYSTEM_ADDRESS,
        HISTORY_STORAGE_ADDRESS,
        parent_block_hash.0.into(),
    ) {
        Ok(res) => res,
        Err(e) => {
            return Err(BlockValidationError::BlockHashContractCall {
                message: e.to_string(),
            }
            .into())
        }
    };

    Ok(Some(res))
}

/// The Canyon hardfork issues an irregular state transition that force-deploys the create2
/// deployer contract. This is done by directly setting the code of the create2 deployer account
/// prior to executing any transactions on the timestamp activation of the fork.
pub(crate) fn ensure_create2_deployer<DB>(
    chain_spec: impl OpHardforks,
    timestamp: u64,
    db: &mut State<DB>,
) -> Result<Option<(AccountInfo, Account)>, DB::Error>
where
    DB: Database,
{
    // If the canyon hardfork is active at the current timestamp, and it was not active at the
    // previous block timestamp (heuristically, block time is not perfectly constant at 2s), and the
    // chain is an optimism chain, then we need to force-deploy the create2 deployer contract.
    if chain_spec.is_canyon_active_at_timestamp(timestamp)
        && !chain_spec.is_canyon_active_at_timestamp(timestamp.saturating_sub(2))
    {
        // Load the create2 deployer account from the cache.
        let acc = db.load_cache_account(CREATE_2_DEPLOYER_ADDR)?;

        // Update the account info with the create2 deployer codehash and bytecode.
        let mut acc_info = acc.account_info().unwrap_or_default();
        let initial_account = acc_info.clone();

        acc_info.code_hash = CREATE_2_DEPLOYER_CODEHASH;
        acc_info.code = Some(Bytecode::new_raw(Bytes::from_static(
            &CREATE_2_DEPLOYER_BYTECODE,
        )));

        // Convert the cache account back into a revm account and mark it as touched.
        let mut revm_acc: revm::state::Account = acc_info.into();
        revm_acc.mark_touch();

        // Commit the create2 deployer account to the database.
        db.commit(HashMap::from_iter([(
            CREATE_2_DEPLOYER_ADDR,
            revm_acc.clone(),
        )]));

        return Ok(Some((initial_account, revm_acc)));
    }

    Ok(None)
}
