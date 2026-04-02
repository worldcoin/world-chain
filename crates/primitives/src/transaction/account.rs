use alloy_primitives::{Address, FixedBytes, address, fixed_bytes};

/// The address of the World ID Account Factory contract.
pub const WORLD_ID_ACCOUNT_FACTORY: Address =
    address!("0x000000000000000000000000000000000000001D");

/// The maximum number of authenticators that can be included in a transaction.
pub const MAX_ACCOUNT_AUTHENTICATORS: usize = 20;

/// The slot used to store the number of authenticators in an account.
///
/// keccak256(""worldchain.world_id_account.max_account_authenticators")
pub const MAX_ACCOUNT_AUTHENTICATORS_SLOT: FixedBytes<32> =
    fixed_bytes!("0x0de429980e94cfff1f418fef336cc19e2a0a57eb14d039a71618a2daeb68d50b");

pub const ACCOUNT_GENERATION_SLOT: FixedBytes<32> =
    fixed_bytes!("0x0de429980e94cfff1f418fef336cc19e2a0a57eb14d039a71618a2daeb68d51c");

/// The slot used to store the key commitment hash.
///
/// keccak256("worldchain.world_id_account.key_commitment")
pub const KEY_COMMITMENT_SLOT: FixedBytes<32> =
    fixed_bytes!("0x5f8b8f2e6e86af0497e8b3a57b60c31c8bdc38cba2a8f3ed68b9a10f7b0c8e1a");

/// Base slot for the key hash membership mapping.
///
/// `mapping_slot(key_hash) = keccak256(abi.encode(key_hash, KEY_HASH_MAPPING_SLOT))`
///
/// Stores `1` for each authorized key hash, enabling O(1) on-chain lookup.
///
/// keccak256("worldchain.world_id_account.key_hash_mapping")
pub const KEY_HASH_MAPPING_SLOT: FixedBytes<32> =
    fixed_bytes!("0x7a2d5f3e1b8c4a0d9e6f2a3b5c7d8e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d");
