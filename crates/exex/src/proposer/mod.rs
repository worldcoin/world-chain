//! OP Proposer ExEx: L2 output submitter ported from `op-proposer`.
//!
//! Pinned to optimism tag [`op-proposer/v1.16.3-rc.1`][tag] (commit
//! `1852be216f45a942321b440da4d92cfb3055f3c1`). The `Mirrors:` annotations
//! throughout these modules reference the upstream Go files below.
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

pub(crate) mod bindings;
pub(crate) mod config;
pub(crate) mod db;
pub(crate) mod driver;
pub(crate) mod exex;
pub(crate) mod local_node;
pub(crate) mod metrics;
pub(crate) mod provider;
pub(crate) mod rpc;
pub(crate) mod service;
pub(crate) mod source;
pub(crate) mod tx;

pub(crate) use bindings::{ContractError, DisputeGameFactory};
