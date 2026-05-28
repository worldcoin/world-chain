//! World Chain ExEx playing the role of the OP Batcher.
//!
//! # Upstream spec reference
//!
//! Consensus-critical encoding (frames, singular batches, channel byte stream)
//! is delegated to **kona-protocol** (`Frame`, `SingleBatch`, `Batch`) pinned to
//! the same optimism tag as the op-reth dependencies. The batcher control flow
//! (`Mirrors:` annotations throughout) tracks `op-batcher` at tag
//! `op-batcher/v1.16.7`, pared to the v1 scope of singular batches, calldata DA,
//! and zlib. Blobs, span batches, brotli, and auto-DA are follow-ups (see
//! `BATCHER_SPEC.md`).
//!
//! ## Internal module → upstream map
//!
//! | Internal module   | Upstream Go file / kona crate                         |
//! | ----------------- | ----------------------------------------------------- |
//! | `config`          | `op-batcher/flags/flags.go`, `op-batcher/batcher/config.go` |
//! | `channel_out`     | `op-node/rollup/derive/channel_out.go` + `kona_protocol` |
//! | `channel`         | `op-batcher/batcher/channel_builder.go`, `channel.go` |
//! | `channel_manager` | `op-batcher/batcher/channel_manager.go`               |
//! | `sync`            | `op-batcher/batcher/sync_actions.go`                  |
//! | `driver`          | `op-batcher/batcher/driver.go`                        |
//! | `service`         | `op-batcher/batcher/service.go`                       |
//! | `rpc`             | `op-batcher/rpc/api.go`                               |
//! | `metrics`         | `op-batcher/metrics/metrics.go`                       |
//! | `source` / `local_node` | (no upstream) local node state reader           |
//! | `provider` / `tx` / `db`| (no upstream) shared with the proposer crate    |

mod channel;
mod channel_manager;
mod channel_out;
mod config;
mod db;
mod driver;
mod error;
mod exex;
mod local_node;
mod metrics;
mod provider;
mod rpc;
mod service;
mod source;
mod sync;
mod tx;

pub use config::{BatcherCliArgs, BatcherConfig, BatcherConfigError};
pub use db::{BatcherStore, StoredBatch, StoredHead};
pub use error::OpBatcherError;
pub use exex::{install_op_batcher_exex, op_batcher_exex};
pub use local_node::{ExExBatcherReader, ProviderBounds};
pub use source::{BlockSourceError, L2BlockData, LocalBlockSource, LocalHeads};

pub type Result<T, E = OpBatcherError> = std::result::Result<T, E>;
