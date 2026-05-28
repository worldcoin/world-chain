//! L2-to-L1 withdrawal types, event decoding, and hashing.
//!
//! Implements the **Cacher** data model from [`wips/wip-1006.md`][wip] and the
//! OP Stack [withdrawals specification][op-withdrawals]. A withdrawal is
//! initiated on L2 by the `L2ToL1MessagePasser` predeploy
//! (`0x4200000000000000000000000000000000000016`), which emits a
//! `MessagePassed` event and writes the withdrawal hash into its
//! `sentMessages` mapping (storage slot `0`).
//!
//! This module provides:
//!
//! * the [`MessagePassed`] `sol!` event binding,
//! * [`WithdrawalTransaction`] — the withdrawal-transaction tuple,
//! * [`withdrawal_hash`] — `keccak256(abi.encode(tx))`, OP's
//!   `Hashing.hashWithdrawal`,
//! * [`message_slot`] — `keccak256(abi.encode(withdrawalHash, uint256(0)))`,
//!   the `sentMessages` mapping slot, and
//! * [`WithdrawalRecord`] / [`WithdrawalStatus`] — the persisted cache record.
//!
//! The relayer driver (proof construction, L1 `proveWithdrawalTransaction` /
//! `finalizeWithdrawalTransaction`, dispute-game polling) is out of scope for
//! the cacher; [`WithdrawalStatus::Proven`] / [`WithdrawalStatus::Finalized`]
//! are forward-compatibility seams it will set.
//!
//! [wip]: ../../../wips/wip-1006.md
//! [op-withdrawals]: https://specs.optimism.io/protocol/withdrawals.html

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::{SolValue, sol};
use serde::{Deserialize, Serialize};

// Re-export the predeploy address rather than redefining it; the proposer's
// local proposal source already owns the canonical constant.
pub use crate::source::local::L2_TO_L1_MESSAGE_PASSER;

sol! {
    /// `MessagePassed` event emitted by the `L2ToL1MessagePasser` predeploy on
    /// every `initiateWithdrawal`.
    ///
    /// Mirrors the upstream `L2ToL1MessagePasser` interface; `withdrawalHash`
    /// is the precomputed `Hashing.hashWithdrawal(tx)` the cacher
    /// independently re-derives and validates.
    #[allow(missing_docs)]
    event MessagePassed(
        uint256 indexed nonce,
        address indexed sender,
        address indexed target,
        uint256 value,
        uint256 gasLimit,
        bytes data,
        bytes32 withdrawalHash
    );
}

/// The fields of an L2-to-L1 withdrawal transaction.
///
/// Matches the `Types.WithdrawalTransaction` struct hashed by
/// `Hashing.hashWithdrawal` in the OP Stack contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawalTransaction {
    pub nonce: U256,
    pub sender: Address,
    pub target: Address,
    pub value: U256,
    pub gas_limit: U256,
    pub data: Bytes,
}

impl WithdrawalTransaction {
    /// Build a [`WithdrawalTransaction`] from a decoded [`MessagePassed`] event.
    pub fn from_event(event: &MessagePassed) -> Self {
        Self {
            nonce: event.nonce,
            sender: event.sender,
            target: event.target,
            value: event.value,
            gas_limit: event.gasLimit,
            data: event.data.clone(),
        }
    }

    /// OP `Hashing.hashWithdrawal`:
    /// `keccak256(abi.encode(nonce, sender, target, value, gasLimit, data))`.
    pub fn hash(&self) -> B256 {
        withdrawal_hash(self)
    }
}

/// OP `Hashing.hashWithdrawal(tx)`:
/// `keccak256(abi.encode(nonce, sender, target, value, gasLimit, data))`.
///
/// The arguments are ABI-encoded as a `(uint256, address, address, uint256,
/// uint256, bytes)` tuple, exactly as `abi.encode` produces them in
/// `Hashing.sol`.
pub fn withdrawal_hash(tx: &WithdrawalTransaction) -> B256 {
    // `abi.encode(a, b, c, ...)` corresponds to alloy's `abi_encode_params`
    // (encode as a parameter list). `abi_encode` would additionally wrap the
    // whole tuple in a leading offset word because it contains a dynamic
    // `bytes` member, which is NOT what Solidity's `abi.encode` produces.
    let encoded = (
        tx.nonce,
        tx.sender,
        tx.target,
        tx.value,
        tx.gas_limit,
        tx.data.clone(),
    )
        .abi_encode_params();
    keccak256(encoded)
}

/// Storage slot of `withdrawalHash` inside the `L2ToL1MessagePasser`
/// `sentMessages` mapping (declared at storage slot `0`):
/// `keccak256(abi.encode(withdrawalHash, uint256(0)))`.
///
/// This is the slot the relayer's storage-inclusion proof targets when
/// building `withdrawalProof`.
pub fn message_slot(withdrawal_hash: B256) -> B256 {
    // Both members are static, so `abi_encode_params` == Solidity
    // `abi.encode(withdrawalHash, uint256(0))`: two consecutive 32-byte words.
    let encoded = (withdrawal_hash, U256::ZERO).abi_encode_params();
    keccak256(encoded)
}

/// Lifecycle status of a cached withdrawal.
///
/// Only [`Cached`](Self::Cached) and [`Orphaned`](Self::Orphaned) are set by
/// the cacher. [`Proven`](Self::Proven) and [`Finalized`](Self::Finalized) are
/// forward-compatibility seams set by the relayer driver once it submits the
/// corresponding L1 transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithdrawalStatus {
    /// Observed on L2 and persisted; not yet proven on L1.
    Cached,
    /// `proveWithdrawalTransaction` included on L1 (relayer-set).
    Proven,
    /// `finalizeWithdrawalTransaction` included on L1 (relayer-set).
    Finalized,
    /// The origin block was reorged out after the withdrawal was already
    /// proven/finalized. Retained for auditing; never re-cached.
    Orphaned,
}

impl WithdrawalStatus {
    /// Whether the withdrawal has progressed beyond the cache on L1.
    ///
    /// Proven/finalized records are never deleted or downgraded by the cacher;
    /// on reorg they are marked [`Orphaned`](Self::Orphaned) instead.
    pub const fn is_settled_on_l1(self) -> bool {
        matches!(self, Self::Proven | Self::Finalized)
    }
}

/// A cached withdrawal record, keyed by its `withdrawalHash`.
///
/// Persisted as JSON in the `WithdrawalStore`. See the field table in
/// [`wips/wip-1006.md`][wip] §Cacher.
///
/// [wip]: ../../../wips/wip-1006.md
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawalRecord {
    /// `MessagePassed.withdrawalHash` (validated against [`withdrawal_hash`]).
    pub withdrawal_hash: B256,
    /// Decoded withdrawal-transaction fields.
    pub tx: WithdrawalTransaction,
    /// L2 block that emitted the event.
    pub l2_block_number: u64,
    /// L2 block hash (for reorg detection).
    pub l2_block_hash: B256,
    /// `sentMessages` mapping slot, [`message_slot`].
    pub message_slot: B256,
    /// Lifecycle status.
    pub status: WithdrawalStatus,
    /// Unix seconds at which the record was first cached.
    pub observed_at_unix: u64,
}

impl WithdrawalRecord {
    /// Build a [`WithdrawalRecord`] from a decoded event and its origin block,
    /// validating the event-supplied hash against the recomputed one.
    ///
    /// Returns `Err(WithdrawalDecodeError::HashMismatch)` if the recomputed
    /// `keccak256(abi.encode(tx))` does not equal `event.withdrawalHash`; the
    /// cacher MUST skip such logs (see [`wips/wip-1006.md`][wip] §Cacher).
    ///
    /// [wip]: ../../../wips/wip-1006.md
    pub fn from_event(
        event: &MessagePassed,
        l2_block_number: u64,
        l2_block_hash: B256,
        observed_at_unix: u64,
    ) -> Result<Self, WithdrawalDecodeError> {
        let tx = WithdrawalTransaction::from_event(event);
        let computed = withdrawal_hash(&tx);
        if computed != event.withdrawalHash {
            return Err(WithdrawalDecodeError::HashMismatch {
                computed,
                event: event.withdrawalHash,
            });
        }
        Ok(Self {
            withdrawal_hash: computed,
            tx,
            l2_block_number,
            l2_block_hash,
            message_slot: message_slot(computed),
            status: WithdrawalStatus::Cached,
            observed_at_unix,
        })
    }
}

/// Error decoding/validating a `MessagePassed` log into a record.
#[derive(Debug, thiserror::Error)]
pub enum WithdrawalDecodeError {
    /// The recomputed withdrawal hash did not match the event-supplied hash.
    #[error("withdrawal hash mismatch: computed {computed}, event {event}")]
    HashMismatch { computed: B256, event: B256 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, hex};

    fn sample_tx() -> WithdrawalTransaction {
        WithdrawalTransaction {
            nonce: U256::from(42u64),
            sender: address!("1111111111111111111111111111111111111111"),
            target: address!("2222222222222222222222222222222222222222"),
            value: U256::from(1_000u64),
            gas_limit: U256::from(100_000u64),
            data: Bytes::from(hex!("deadbeef")),
        }
    }

    /// `withdrawal_hash` must equal the manual `abi.encode` + keccak256 of the
    /// six-field tuple, matching OP `Hashing.hashWithdrawal`.
    #[test]
    fn withdrawal_hash_matches_abi_encode() {
        let tx = sample_tx();
        let manual = keccak256(
            (
                tx.nonce,
                tx.sender,
                tx.target,
                tx.value,
                tx.gas_limit,
                tx.data.clone(),
            )
                .abi_encode_params(),
        );
        assert_eq!(withdrawal_hash(&tx), manual);
        // Deterministic.
        assert_eq!(withdrawal_hash(&tx), withdrawal_hash(&tx));
    }

    /// A withdrawal with empty calldata (the common ETH-only case) hashes the
    /// dynamic `bytes` tail correctly. `abi.encode` (== `abi_encode_params`)
    /// lays out 5 static words + an offset word pointing at the dynamic
    /// `bytes`, then a length word for the empty `bytes` = `7 * 32` bytes. (The
    /// plain `abi_encode` would prepend an extra wrapping offset for the whole
    /// tuple — which is exactly the layout we must NOT use.)
    #[test]
    fn withdrawal_hash_empty_data_encoding_length() {
        let tx = WithdrawalTransaction {
            data: Bytes::new(),
            ..sample_tx()
        };
        let encoded = (
            tx.nonce,
            tx.sender,
            tx.target,
            tx.value,
            tx.gas_limit,
            tx.data.clone(),
        )
            .abi_encode_params();
        // 5 static words + offset word for `bytes` + length word.
        assert_eq!(encoded.len(), 7 * 32);
        assert_eq!(withdrawal_hash(&tx), keccak256(&encoded));
    }

    /// `message_slot` must equal `keccak256(abi.encode(hash, uint256(0)))`,
    /// the canonical mapping-slot derivation for `sentMessages` at slot 0.
    #[test]
    fn message_slot_matches_mapping_derivation() {
        let h = B256::repeat_byte(0xab);
        let manual = keccak256((h, U256::ZERO).abi_encode_params());
        assert_eq!(message_slot(h), manual);
        // The encoded preimage is exactly two 32-byte words.
        assert_eq!((h, U256::ZERO).abi_encode_params().len(), 64);
    }

    /// `WithdrawalRecord::from_event` rejects a log whose supplied hash does
    /// not match the recomputed hash.
    #[test]
    fn from_event_rejects_hash_mismatch() {
        let tx = sample_tx();
        let event = MessagePassed {
            nonce: tx.nonce,
            sender: tx.sender,
            target: tx.target,
            value: tx.value,
            gasLimit: tx.gas_limit,
            data: tx.data.clone(),
            withdrawalHash: B256::ZERO, // deliberately wrong
        };
        let err = WithdrawalRecord::from_event(&event, 10, B256::repeat_byte(1), 0).unwrap_err();
        assert!(matches!(err, WithdrawalDecodeError::HashMismatch { .. }));
    }

    /// A well-formed event yields a `Cached` record with the derived slot.
    #[test]
    fn from_event_accepts_valid() {
        let tx = sample_tx();
        let hash = withdrawal_hash(&tx);
        let event = MessagePassed {
            nonce: tx.nonce,
            sender: tx.sender,
            target: tx.target,
            value: tx.value,
            gasLimit: tx.gas_limit,
            data: tx.data.clone(),
            withdrawalHash: hash,
        };
        let rec = WithdrawalRecord::from_event(&event, 10, B256::repeat_byte(1), 123).unwrap();
        assert_eq!(rec.withdrawal_hash, hash);
        assert_eq!(rec.message_slot, message_slot(hash));
        assert_eq!(rec.status, WithdrawalStatus::Cached);
        assert_eq!(rec.l2_block_number, 10);
    }

    /// Frozen known-answer vectors for the all-zero withdrawal. These pin the
    /// exact `abi.encode` layout (the dynamic-`bytes` tail in particular) so a
    /// future change to the encoding — e.g. accidentally switching to the
    /// tuple-wrapping `abi_encode` — is caught immediately.
    ///
    /// The all-zero withdrawal hashes the canonical 7-word preimage
    /// `[0;32]*5 || offset(192) || len(0)`; the `sentMessages` slot for the
    /// zero hash is `keccak256([0;32] || [0;32])`.
    #[test]
    fn known_answer_zero_vector() {
        let tx = WithdrawalTransaction {
            nonce: U256::ZERO,
            sender: Address::ZERO,
            target: Address::ZERO,
            value: U256::ZERO,
            gas_limit: U256::ZERO,
            data: Bytes::new(),
        };
        assert_eq!(
            withdrawal_hash(&tx),
            B256::from(hex!(
                "fb9a0e4cccc08415f2a908602092a297f2bde08dcf758c7952e04f3de9ebc48f"
            )),
        );
        assert_eq!(
            message_slot(B256::ZERO),
            B256::from(hex!(
                "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5"
            )),
        );
    }
}
