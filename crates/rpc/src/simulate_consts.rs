//! Constants used by `simulate_unsignedUserOp` — gas/timeout limits, event
//! signatures, proxy storage slots, revert decoding selectors, and metadata
//! view selectors.

use alloy_primitives::B256;
use std::time::Duration;

// ─── Gas / cache / timeout limits ────────────────────────────────────────────

/// Maximum gas limit accepted for a simulated call.
pub const MAX_SIMULATION_GAS: u64 = 8_000_000;

/// Bounded to prevent unbounded memory growth from adversarial UserOps that
/// emit Transfer events from many fresh token contracts.
pub(crate) const METADATA_CACHE_CAPACITY: usize = 1000;

/// Hard wall-clock cap on a single simulation, observed by the client. Beyond
/// this we return `internal error: simulation deadline exceeded`. The blocking
/// task continues to drain on the rayon pool until the EVM finishes (bounded
/// by `MAX_SIMULATION_GAS`), holding its concurrency permit until then so the
/// guard correctly accounts for slow simulations.
pub const SIMULATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of (id, value) pairs accepted in a single ERC-1155 TransferBatch log.
///
/// Bounded to prevent adversarially crafted logs from forcing huge allocations
/// in `decode_batch_transfer_data`.
pub const MAX_BATCH_TRANSFERS: usize = 1000;

// ─── Asset / approval event topics ───────────────────────────────────────────

/// `Transfer(address,address,uint256)` — ERC-20 and ERC-721
pub(crate) const TRANSFER_TOPIC: B256 =
    alloy_primitives::b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

/// `TransferSingle(address,address,address,uint256,uint256)` — ERC-1155
pub(crate) const TRANSFER_SINGLE_TOPIC: B256 =
    alloy_primitives::b256!("c3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62");

/// `TransferBatch(address,address,address,uint256[],uint256[])` — ERC-1155
pub(crate) const TRANSFER_BATCH_TOPIC: B256 =
    alloy_primitives::b256!("4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb");

/// `Approval(address,address,uint256)` — ERC-20 and ERC-721
pub(crate) const APPROVAL_TOPIC: B256 =
    alloy_primitives::b256!("8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925");

/// `ApprovalForAll(address,address,bool)` — ERC-721 / ERC-1155
pub(crate) const APPROVAL_FOR_ALL_TOPIC: B256 =
    alloy_primitives::b256!("17307eab39ab6107e8899845ad3d59bd9653f200f220920489ca2b5937696c31");

// ─── Contract management event topics ────────────────────────────────────────

/// `Upgraded(address)` — EIP-1967 / UUPS implementation upgrade.
pub(crate) const UPGRADED_TOPIC: B256 =
    alloy_primitives::b256!("bc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b");

/// `BeaconUpgraded(address)` — EIP-1967 beacon proxy upgrade.
pub(crate) const BEACON_UPGRADED_TOPIC: B256 =
    alloy_primitives::b256!("1cf3b03a6cf19fa2baba4df148e9dcabedea7f8a5c07840e207e5c089be95d3e");

/// `AdminChanged(address,address)` — EIP-1967 proxy admin rotation. A
/// privilege-escalation vector: the new admin can swap the implementation.
pub(crate) const ADMIN_CHANGED_TOPIC: B256 =
    alloy_primitives::b256!("7e644d79422f17c01e4894b5f4f588d331ebfa28653d42ae832dc59e38c9798f");

/// `OwnershipTransferred(address,address)` — OpenZeppelin `Ownable`.
pub(crate) const OWNERSHIP_TRANSFERRED_TOPIC: B256 =
    alloy_primitives::b256!("8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e0");

/// Safe `AddedOwner(address)`.
pub(crate) const SAFE_ADDED_OWNER_TOPIC: B256 =
    alloy_primitives::b256!("9465fa0c962cc76958e6373a993326400c1c94f8be2fe3a952adfa7f60b2ea26");

/// Safe `RemovedOwner(address)`.
pub(crate) const SAFE_REMOVED_OWNER_TOPIC: B256 =
    alloy_primitives::b256!("f8d49fc529812e9a7c5c50e69c20f0dccc0db8fa95c98bc58cc9a4f1c1299eaf");

/// Safe `ChangedThreshold(uint256)`.
pub(crate) const SAFE_CHANGED_THRESHOLD_TOPIC: B256 =
    alloy_primitives::b256!("610f7ff2b304ae8903c3de74c60c6ab1f7d6226b3f52c5161905bb5ad4039c93");

/// Safe `EnabledModule(address)`.
pub(crate) const SAFE_ENABLED_MODULE_TOPIC: B256 =
    alloy_primitives::b256!("ecdf3a3effea5783a3c4c2140e677577666428d44ed9d474a0b3a4c9943f8440");

/// Safe `DisabledModule(address)`.
pub(crate) const SAFE_DISABLED_MODULE_TOPIC: B256 =
    alloy_primitives::b256!("aab4fa2b463f581b2b32cb3b7e3b704b9ce37cc209b5fb4d77e593ace4054276");

/// `DiamondCut((address,uint8,bytes4[])[],address,bytes)` — EIP-2535 facet changes.
pub(crate) const DIAMOND_CUT_TOPIC: B256 =
    alloy_primitives::b256!("8faa70878671ccd212d20771b795c50af8fd3ff6cf27f4bde57e5d4de0aeb673");

// ─── Proxy storage slots (signature-agnostic upgrade detection) ──────────────

/// EIP-1967 implementation slot — `keccak256("eip1967.proxy.implementation") - 1`.
pub(crate) const EIP1967_IMPLEMENTATION_SLOT: B256 =
    alloy_primitives::b256!("360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc");

/// EIP-1967 beacon slot — `keccak256("eip1967.proxy.beacon") - 1`.
pub(crate) const EIP1967_BEACON_SLOT: B256 =
    alloy_primitives::b256!("a3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50");

/// EIP-1967 admin slot — `keccak256("eip1967.proxy.admin") - 1`.
pub(crate) const EIP1967_ADMIN_SLOT: B256 =
    alloy_primitives::b256!("b53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103");

/// EIP-1822 ("Proxiable") implementation slot — `keccak256("PROXIABLE")`.
pub(crate) const EIP1822_PROXIABLE_SLOT: B256 =
    alloy_primitives::b256!("c5f16f0fcc639fa48a6947836d9850f504798523bf8c9a3a87d5876cf622bcf7");

/// Legacy OpenZeppelin implementation slot — `keccak256("org.zeppelinos.proxy.implementation")`.
pub(crate) const OZ_LEGACY_IMPLEMENTATION_SLOT: B256 =
    alloy_primitives::b256!("7050c9e0f4ca769c69bd3a8ef740bc37934f8e2c036e5a723fd8ee048ed3f8c3");

// ─── Revert reason decoding ──────────────────────────────────────────────────

/// `Error(string)` selector — `keccak256("Error(string)")[..4]`.
pub(crate) const ERROR_STRING_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];

/// `Panic(uint256)` selector — `keccak256("Panic(uint256)")[..4]`.
pub(crate) const PANIC_UINT256_SELECTOR: [u8; 4] = [0x4e, 0x48, 0x7b, 0x71];

/// Minimum payload length for a well-formed `Error(string)` revert:
/// selector(4) + string-offset(32) + string-length(32).
pub(crate) const MIN_ERROR_STRING_LEN: usize = 4 + 32 + 32;

/// Minimum payload length for a well-formed `Panic(uint256)` revert:
/// selector(4) + uint256(32).
pub(crate) const MIN_PANIC_UINT256_LEN: usize = 4 + 32;

// ─── ERC-20 metadata view selectors ──────────────────────────────────────────

/// Well-known ERC-20 metadata view selectors. Defined here so they're shared
/// between trace decoding (`selector_to_name`) and the metadata resolver, which
/// needs the raw bytes to make the calls.
pub const NAME_SELECTOR: [u8; 4] = [0x06, 0xfd, 0xde, 0x03]; // name()
pub const SYMBOL_SELECTOR: [u8; 4] = [0x95, 0xd8, 0x9b, 0x41]; // symbol()
pub const DECIMALS_SELECTOR: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67]; // decimals()
