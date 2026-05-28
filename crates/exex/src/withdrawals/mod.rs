//! Withdrawal Cacher and Relayer ExEx.
//!
//! Implements the **Cacher** role: it observes
//! committed L2 blocks through the ExEx notification stream, extracts
//! `MessagePassed` events emitted by the `L2ToL1MessagePasser` predeploy, and
//! persists each withdrawal (reorg-aware) so it can later be proven and
//! finalized on L1.
//!
//! ## Internal modules
//!
//! | Internal module | Role                                                    |
//! | --------------- | ------------------------------------------------------- |
//! | `types`         | Withdrawal types, `MessagePassed` event, hashing.       |
//! | `store`         | MDBX-backed, reorg-aware withdrawal cache.              |
//! | `cacher`        | Chain-scanning core (decoupled from the store).         |
//! | `config`        | `--relayer.*` CLI args (cacher subset).                 |
//! | `exex`          | reth ExEx entrypoint + notification loop.               |

pub(crate) mod cacher;
pub(crate) mod config;
pub(crate) mod exex;
pub(crate) mod store;
pub(crate) mod types;
