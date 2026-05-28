//! World Chain ExEx playing the role of the OP Proposer.
//!
//! # Upstream spec reference
//!
//! All `Mirrors:` annotations on functions throughout this crate are pinned to
//! optimism tag [`op-proposer/v1.16.3-rc.1`][tag] (commit
//! `1852be216f45a942321b440da4d92cfb3055f3c1`).
//!
//! [tag]: https://github.com/ethereum-optimism/optimism/tree/op-proposer/v1.16.3-rc.1
//!
//! ## Internal module → upstream file map
//!
//! | Internal module      | Upstream Go file                                  |
//! | -------------------- | ------------------------------------------------- |
//! | `config`             | `op-proposer/flags/flags.go`, `op-proposer/proposer/config.go` |
//! | `bindings`           | `op-proposer/contracts/disputegamefactory.go`     |
//! | `driver`             | `op-proposer/proposer/driver.go`                  |
//! | `metrics`            | `op-proposer/metrics/metrics.go`                  |
//! | `rpc`                | `op-proposer/proposer/rpc/api.go`                 |
//! | `service`            | `op-proposer/proposer/service.go`                 |
//! | `source`             | `op-proposer/proposer/source/source.go`           |
//! | `source::rollup`     | `op-proposer/proposer/source/source_rollup.go`    |
//! | `source::local`      | `op-service/eth/output.go` (`OutputV0`)           |

mod config;
mod bindings;
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
mod tx;

pub use config::{ProposerCliArgs, ProposerConfig};
pub use db::{ProposerStore, StoredHead, StoredProposal};
pub use error::OpProposerError;
pub use exex::{install_op_proposer_exex, op_proposer_exex};
pub use local_node::{ExExChainReader, ProviderBounds};
pub use provider::{L1Provider, L1ProviderConfig, SignerKind};
pub use service::{AdminRpcSettings, ProposerService};
use bindings::{DisputeGameFactory, ContractError};
pub use source::{
    Proposal, ProposalSource, ProposalSourceError, SyncStatus,
    local::{
        BlockMeta, ChainStatus, L2_TO_L1_MESSAGE_PASSER, LocalProposalSource, LocalStorageReader,
    },
};

pub type Result<T, E = OpProposerError> = std::result::Result<T, E>;
