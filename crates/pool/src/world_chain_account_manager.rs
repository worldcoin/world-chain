//! Typed reader over the WIP-1001 `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy.
//!
//! This module isolates the raw storage-slot derivation and bit-packing rules
//! described in WIP-1001 behind a small reader type, so callers do not need to
//! hand-roll `keccak256`/shift/mask arithmetic against the predeploy.
//!
//! The constants below MUST match the storage layout of
//! `pkg/contracts/src/WorldChainAccountManager.sol`. That contract is currently
//! `TODO: FIXME`. The values here encode the layout we expect once it lands
//! and are pinned by the unit tests in this module.
//!
//! Expected Solidity layout:
//!
//! ```solidity
//! contract WorldChainAccountManager {
//!     mapping(address => Account) internal accounts; // slot 0
//! }
//!
//! struct Account {
//!     WorldChainAccountVerifier admin;             // slots base + [0..1]
//!                                                  //   .verifier  : address       (slot base + 0)
//!                                                  //   .installation: bytes header (slot base + 1)
//!     bytes32 accountSalt;                         // slot base + 2
//!     WorldChainAccountVerifier[] sessionVerifiers;// slot base + 3 (length)
//!     bytes32 keyRingHash;                         // slot base + 4
//!     uint64 adminNonce;                           // slot base + 5, bits   [0..63]
//!     uint64 transactionNonce;                     // slot base + 5, bits  [64..127]
//! }
//! ```
//!
//! For a given world-chain account address `a`, the base slot of
//! `accounts[a]` is `keccak256(abi.encode(a, ACCOUNTS_MAPPING_SLOT))`.

use alloy_primitives::{Address, keccak256};
use alloy_sol_types::SolValue;
use reth_provider::{ProviderResult, StateProvider};
use revm_primitives::U256;

/// Mask covering the 64 bits occupied by a single nonce field inside the
/// packed `adminNonce`/`transactionNonce` slot.
const U64_MASK: U256 = U256::from_limbs([u64::MAX, 0, 0, 0]);

/// Typed reader over `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy storage.
///
/// Wraps a [`StateProvider`] and exposes high-level accessors for the fields
/// the mempool needs from the predeploy, hiding the storage-slot derivation
/// and bit-packing details described in WIP-1001.
#[derive(Debug)]
pub struct WorldChainAccountManagerReader<'s, S: ?Sized> {
    state: &'s S,
}

impl<'s, S: StateProvider + ?Sized> WorldChainAccountManagerReader<'s, S> {
    /// The address of the WIP-1001 `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy.
    ///
    /// Per WIP-1001 "Activation Parameters" this is TBD in the spec; tracking
    /// `Address::ZERO` here until the activation parameter is assigned.
    // TODO: replace with the canonical predeploy address once WIP-1001 fork
    // configuration assigns it.
    pub const ADDRESS: Address = Address::ZERO;

    /// Storage slot of the `accounts` mapping inside `WorldChainAccountManager`.
    // TODO: confirm against the Solidity layout once `WorldChainAccountManager.sol`
    // is implemented.
    pub const ACCOUNTS_MAPPING_SLOT: U256 = U256::ZERO;

    /// Offset, in slots, of the slot that packs `adminNonce` and
    /// `transactionNonce` within the per-account `Account` struct.
    // TODO: confirm against the Solidity layout once `WorldChainAccountManager.sol`
    // is implemented.
    pub const ACCOUNT_NONCE_PACKED_SLOT_OFFSET: u64 = 5;

    /// Bit-offset of the `transactionNonce` field within `accounts[a]`'s
    /// nonce-packed slot. Solidity packs the first-declared `adminNonce` into
    /// the low 64 bits, putting `transactionNonce` at offset 64.
    pub const ACCOUNT_TX_NONCE_BIT_OFFSET: u32 = 64;

    /// Bit-offset of the `adminNonce` field within `accounts[a]`'s
    /// nonce-packed slot (low 64 bits).
    pub const ACCOUNT_ADMIN_NONCE_BIT_OFFSET: u32 = 0;

    /// Bind a reader to a state provider.
    pub fn new(state: &'s S) -> Self {
        Self { state }
    }

    /// Storage slot holding `accounts[account].adminNonce` and
    /// `.transactionNonce` (both packed in the same slot).
    pub fn account_nonce_slot(account: Address) -> U256 {
        let key_encoded = (account, Self::ACCOUNTS_MAPPING_SLOT).abi_encode();
        let base_slot = U256::from_be_bytes::<32>(keccak256(&key_encoded).0);
        base_slot + U256::from(Self::ACCOUNT_NONCE_PACKED_SLOT_OFFSET)
    }

    /// Reads `accounts[account].transactionNonce` from predeploy storage.
    pub fn transaction_nonce(&self, account: Address) -> ProviderResult<u64> {
        let raw = self.read_account_nonce_packed_slot(account)?;
        Ok(Self::extract_nonce(raw, Self::ACCOUNT_TX_NONCE_BIT_OFFSET))
    }

    /// Reads `accounts[account].adminNonce` from predeploy storage.
    pub fn admin_nonce(&self, account: Address) -> ProviderResult<u64> {
        let raw = self.read_account_nonce_packed_slot(account)?;
        Ok(Self::extract_nonce(
            raw,
            Self::ACCOUNT_ADMIN_NONCE_BIT_OFFSET,
        ))
    }

    /// Reads the raw storage word holding the packed `adminNonce` and
    /// `transactionNonce` for `account`. Defaults to `U256::ZERO` when the
    /// slot is unset, matching how the EVM exposes cold storage.
    fn read_account_nonce_packed_slot(&self, account: Address) -> ProviderResult<U256> {
        let slot = Self::account_nonce_slot(account);
        Ok(self
            .state
            .storage(Self::ADDRESS, slot.into())?
            .unwrap_or_default())
    }

    /// Extracts a 64-bit nonce field from a packed storage word at the given
    /// bit offset.
    fn extract_nonce(packed: U256, bit_offset: u32) -> u64 {
        ((packed >> bit_offset) & U64_MASK).to()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    /// Convenience alias so the tests don't have to spell out a concrete
    /// `StateProvider` just to exercise the associated functions / constants.
    type Reader<'s> = WorldChainAccountManagerReader<'s, dyn StateProvider>;

    /// `keccak256(abi.encode(addr, 0))` for a fixed address — pinned vector
    /// so future changes to the slot derivation surface as a test failure.
    #[test]
    fn account_nonce_slot_matches_known_vector() {
        let acc = address!("000000000000000000000000000000000000001d");
        let slot = Reader::account_nonce_slot(acc);

        // base = keccak256(abi.encode(acc, uint256(0))) computed via the same
        // primitive used in the helper, but spelled out independently here so
        // a regression in the helper would diverge from this hand-rolled
        // expectation.
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(acc.as_slice());
        let base = U256::from_be_bytes::<32>(keccak256(buf).0);
        let expected = base + U256::from(Reader::ACCOUNT_NONCE_PACKED_SLOT_OFFSET);

        assert_eq!(slot, expected);
    }

    #[test]
    fn account_nonce_slot_is_deterministic_per_address() {
        let a = address!("000000000000000000000000000000000000001d");
        let b = address!("00000000000000000000000000000000000000aa");
        assert_eq!(Reader::account_nonce_slot(a), Reader::account_nonce_slot(a));
        assert_ne!(Reader::account_nonce_slot(a), Reader::account_nonce_slot(b));
    }

    #[test]
    fn nonces_extracted_from_packed_slot() {
        // Build a packed slot value: adminNonce = 0x1122334455667788,
        // transactionNonce = 0xaabbccddeeff0011. Solidity packs the first-
        // declared field into the low bits, so the slot word is
        // (transactionNonce << 64) | adminNonce.
        let admin_nonce: u64 = 0x1122_3344_5566_7788;
        let tx_nonce: u64 = 0xaabb_ccdd_eeff_0011;

        let slot_value =
            (U256::from(tx_nonce) << Reader::ACCOUNT_TX_NONCE_BIT_OFFSET) | U256::from(admin_nonce);

        assert_eq!(
            Reader::extract_nonce(slot_value, Reader::ACCOUNT_TX_NONCE_BIT_OFFSET),
            tx_nonce,
        );
        assert_eq!(
            Reader::extract_nonce(slot_value, Reader::ACCOUNT_ADMIN_NONCE_BIT_OFFSET),
            admin_nonce,
        );
    }

    #[test]
    fn nonce_packed_slot_layout_constants_are_in_sync() {
        // These constants form a tight contract; pin them so accidental edits
        // require touching the layout doc-block above too.
        assert_eq!(Reader::ACCOUNT_NONCE_PACKED_SLOT_OFFSET, 5);
        assert_eq!(Reader::ACCOUNT_TX_NONCE_BIT_OFFSET, 64);
        assert_eq!(Reader::ACCOUNT_ADMIN_NONCE_BIT_OFFSET, 0);
        assert_eq!(U64_MASK, U256::from(u64::MAX));
        assert_eq!(Reader::ACCOUNTS_MAPPING_SLOT, U256::ZERO);
        assert_eq!(Reader::ADDRESS, Address::ZERO);
    }
}
