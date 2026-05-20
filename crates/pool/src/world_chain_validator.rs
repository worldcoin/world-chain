//! This file will replace the validator.rs file once I create the WorldChainPrimitives type and
//! wire it everywhere it's needed. This also makes it possible to keep our existent codebase
//! unaltered so that we don't change our current functionalities while developing wip1001 features.

use crate::world_chain_tx::{WorldChainPoolTransaction, WorldChainPoolTransactionError};
use alloy_primitives::{Address, keccak256};
use alloy_sol_types::SolValue;
use reth_evm::ConfigureEvm;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_primitives_traits::{
    BlockTy, GotExpected, TxTy, transaction::error::InvalidTransactionError,
};
use reth_provider::{BlockReaderIdExt, ChainSpecProvider, StateProviderFactory};
use reth_transaction_pool::{
    TransactionOrigin, TransactionValidationOutcome, TransactionValidator,
};
use revm_primitives::U256;
use world_chain_primitives::transaction::WIP_1001_TX_TYPE;

/// The min validation failure fee that world chain account MUST have for
/// a WIP-1001 tx to be considered valid and inserted into the mempool.
// TODO: change this value to an appropriate one!!!
pub const MIN_VALIDATION_FAILURE_FEE: U256 = U256::from_limbs([0xFFFF, 0, 0, 0]);

// ---------------------------------------------------------------------------
// WIP-1001 `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy storage layout
// ---------------------------------------------------------------------------
//
// The constants below MUST match the storage layout of
// `pkg/contracts/src/WorldChainAccountManager.sol`. That contract is currently
// `TODO: FIXME`. The values here encode the layout we expect once it lands and
// are pinned by a Rust unit test (`storage_layout::tests`).
//
// Expected Solidity layout:
//
//   contract WorldChainAccountManager {
//       mapping(address => Account) internal accounts; // slot 0
//   }
//
//   struct Account {
//       WorldChainAccountVerifier admin;             // slots base + [0..1]
//                                                    //   .verifier  : address       (slot base + 0)
//                                                    //   .installation: bytes header (slot base + 1)
//       bytes32 accountSalt;                         // slot base + 2
//       WorldChainAccountVerifier[] sessionVerifiers;// slot base + 3 (length)
//       bytes32 keyRingHash;                         // slot base + 4
//       uint64 adminNonce;                           // slot base + 5, bits   [0..63]
//       uint64 transactionNonce;                     // slot base + 5, bits  [64..127]
//   }
//
// For a given world-chain account address `a`, the base slot of
// `accounts[a]` is `keccak256(abi.encode(a, ACCOUNTS_MAPPING_SLOT))`.

/// The address of the WIP-1001 `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy.
///
/// Per WIP-1001 "Activation Parameters" this is TBD in the spec; tracking
/// `Address::ZERO` here until the activation parameter is assigned.
// TODO: replace with the canonical predeploy address once WIP-1001 fork
// configuration assigns it.
pub const WORLD_CHAIN_ACCOUNT_MANAGER: Address = Address::ZERO;

/// Storage slot of the `accounts` mapping inside `WorldChainAccountManager`.
// TODO: confirm against the Solidity layout once `WorldChainAccountManager.sol`
// is implemented.
pub const WORLD_CHAIN_ACCOUNTS_MAPPING_SLOT: U256 = U256::ZERO;

/// Offset, in slots, of the slot that packs `adminNonce` and
/// `transactionNonce` within the per-account `Account` struct.
// TODO: confirm against the Solidity layout once `WorldChainAccountManager.sol`
// is implemented.
pub const WORLD_CHAIN_ACCOUNT_NONCE_PACKED_SLOT_OFFSET: u64 = 5;

/// Bit-offset of the `transactionNonce` field within
/// `accounts[a]`'s nonce-packed slot. Solidity packs the first-declared
/// `adminNonce` into the low 64 bits, putting `transactionNonce` at offset 64.
pub const WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET: u32 = 64;

/// Mask for the 64 bits occupied by `transactionNonce` inside its slot.
pub const U64_MASK: U256 = U256::from_limbs([u64::MAX, 0, 0, 0]);

/// Computes the storage slot holding `accounts[world_chain_account].adminNonce`
/// and `.transactionNonce` (both packed in the same slot).
fn world_chain_account_nonce_slot(world_chain_account: Address) -> U256 {
    let key_encoded = (world_chain_account, WORLD_CHAIN_ACCOUNTS_MAPPING_SLOT).abi_encode();
    let base_slot = U256::from_be_bytes::<32>(keccak256(&key_encoded).0);
    base_slot + U256::from(WORLD_CHAIN_ACCOUNT_NONCE_PACKED_SLOT_OFFSET)
}

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    /// The inner transaction validator.
    inner: OpTransactionValidator<Client, Tx, Evm>,
}

impl<Client, Tx, Evm> WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: ChainSpecProvider<ChainSpec: OpHardforks>
        + StateProviderFactory
        + BlockReaderIdExt<Block = BlockTy<Evm::Primitives>>,
    Tx: WorldChainPoolTransaction<Consensus = TxTy<Evm::Primitives>>,
    Evm: ConfigureEvm,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(inner: OpTransactionValidator<Client, Tx, Evm>) -> Self {
        Self { inner }
    }

    /// Get a reference to the inner transaction validator.
    pub fn inner(&self) -> &OpTransactionValidator<Client, Tx, Evm> {
        &self.inner
    }

    pub async fn validate_wip1001(
        &self,
        _origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        let state = match self.inner.client().latest() {
            Ok(new_state) => Box::new(new_state),
            Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
        };
        // check whether world chain account exists
        // TODO: it's better to call WorldChainAccountManager.getKeyRingHash(account)
        // and ensure it's not zero, therefore we're sure that account is a valid and esistent
        // world chain account.
        let world_chain_account_addr = tx.sender();
        let world_chain_account = match state.basic_account(&world_chain_account_addr) {
            Ok(maybe_account) => match maybe_account {
                Some(account) => account,
                None => {
                    return WorldChainPoolTransactionError::WorldChainAccountDoesNotExist(
                        world_chain_account_addr,
                    )
                    .to_outcome(tx);
                }
            },
            Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
        };
        // world chain account balance must cover `MIN_VALIDATION_FAILURE_FEE`
        if world_chain_account.balance < MIN_VALIDATION_FAILURE_FEE {
            return TransactionValidationOutcome::Invalid(
                tx,
                InvalidTransactionError::InsufficientFunds(
                    GotExpected {
                        got: world_chain_account.balance,
                        expected: MIN_VALIDATION_FAILURE_FEE,
                    }
                    .into(),
                )
                .into(),
            );
        }
        // tx nonce should be greater or equal to the account.transactionNonce
        let tx_nonce = tx.nonce();
        let nonce_slot = world_chain_account_nonce_slot(world_chain_account_addr);
        let account_tx_nonce: u64 =
            match state.storage(WORLD_CHAIN_ACCOUNT_MANAGER, nonce_slot.into()) {
                Ok(maybe_slot) => ((maybe_slot.unwrap_or_default()
                    >> WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET)
                    & U64_MASK)
                    .to(),
                Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
            };
        if tx_nonce < account_tx_nonce {
            return WorldChainPoolTransactionError::StaleTransactionNonce {
                got: tx_nonce,
                expected: account_tx_nonce,
            }
            .to_outcome(tx);
        }
        // check whether session verifier is included in the authorized set
        todo!()
    }
}

impl<Client, Tx, Evm> TransactionValidator for WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: ChainSpecProvider<ChainSpec: OpHardforks>
        + StateProviderFactory
        + BlockReaderIdExt<Block = BlockTy<Evm::Primitives>>,
    Tx: WorldChainPoolTransaction<Consensus = TxTy<Evm::Primitives>>,
    Evm: ConfigureEvm,
{
    type Transaction = Tx;

    type Block = BlockTy<Evm::Primitives>;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        if transaction.ty() == WIP_1001_TX_TYPE {
            // Note: this line is currently unreachable because we're still using legacy
            // OpPooledTransaction that doesn't have the wip1001 tx type, therefore
            // wip1001 txs would fail at the decoding phase, never reaching this point.
            // See: https://vscode.dev/github/worldcoin/world-chain/blob/ale/wip1001-mempool-validation/crates/pool/src/tx.rs#L232
            return self.validate_wip1001(origin, transaction.clone()).await;
        }
        return self.inner.validate_one(origin, transaction.clone()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    /// `keccak256(abi.encode(addr, 0))` for a fixed address — pinned vector so
    /// future changes to the slot derivation surface as a test failure.
    #[test]
    fn account_nonce_slot_matches_known_vector() {
        let acc = address!("000000000000000000000000000000000000001d");
        let slot = world_chain_account_nonce_slot(acc);

        // base = keccak256(abi.encode(acc, uint256(0))) computed via the same
        // primitive used in the helper, but spelled out independently here so
        // a regression in the helper would diverge from this hand-rolled
        // expectation.
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(acc.as_slice());
        let base = U256::from_be_bytes::<32>(keccak256(buf).0);
        let expected = base + U256::from(WORLD_CHAIN_ACCOUNT_NONCE_PACKED_SLOT_OFFSET);

        assert_eq!(slot, expected);
    }

    #[test]
    fn account_nonce_slot_is_deterministic_per_address() {
        let a = address!("000000000000000000000000000000000000001d");
        let b = address!("00000000000000000000000000000000000000aa");
        assert_eq!(
            world_chain_account_nonce_slot(a),
            world_chain_account_nonce_slot(a)
        );
        assert_ne!(
            world_chain_account_nonce_slot(a),
            world_chain_account_nonce_slot(b)
        );
    }

    #[test]
    fn transaction_nonce_extracted_from_packed_slot() {
        // Build a packed slot value: adminNonce = 0x1122334455667788, transactionNonce = 0xaabbccddeeff0011.
        // Solidity packs first-declared into the low bits, so the slot word is
        // (transactionNonce << 64) | adminNonce.
        let admin_nonce: u64 = 0x1122_3344_5566_7788;
        let tx_nonce: u64 = 0xaabb_ccdd_eeff_0011;

        let slot_value = (U256::from(tx_nonce) << WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET)
            | U256::from(admin_nonce);

        let extracted: u64 =
            ((slot_value >> WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET) & U64_MASK).to();
        assert_eq!(extracted, tx_nonce);

        // Sanity-check the `adminNonce` half too (low 64 bits).
        let admin_extracted: u64 = (slot_value & U64_MASK).to();
        assert_eq!(admin_extracted, admin_nonce);
    }

    #[test]
    fn nonce_packed_slot_layout_constants_are_in_sync() {
        // These constants form a tight contract; pin them so accidental edits
        // require touching the layout doc-block above too.
        assert_eq!(WORLD_CHAIN_ACCOUNT_NONCE_PACKED_SLOT_OFFSET, 5);
        assert_eq!(WORLD_CHAIN_ACCOUNT_TX_NONCE_BIT_OFFSET, 64);
        assert_eq!(U64_MASK, U256::from(u64::MAX));
        assert_eq!(WORLD_CHAIN_ACCOUNTS_MAPPING_SLOT, U256::ZERO);
        assert_eq!(WORLD_CHAIN_ACCOUNT_MANAGER, Address::ZERO);
    }
}
