//! World Chain ExEx crate.
//!
//! Hosts two cooperating, independently-gated reth Execution Extensions:
//!
//! * [`proposer`] — the **OP Proposer** (L2 output submitter), ported from
//!   `op-proposer` and pinned to optimism tag
//!   [`op-proposer/v1.16.3-rc.1`][tag] (commit
//!   `1852be216f45a942321b440da4d92cfb3055f3c1`). All `Mirrors:` annotations
//!   in that module reference the upstream Go files.
//! * [`withdrawals`] — the **Withdrawal Cacher and Relayer**, implementing the
//!   cacher role of [`wips/wip-1006.md`][wip].
//!
//! [`error`] holds the shared top-level [`OpProposerError`] returned from the
//! crate's public surface.
//!
//! [tag]: https://github.com/ethereum-optimism/optimism/tree/op-proposer/v1.16.3-rc.1
//! [wip]: ../../../wips/wip-1006.md
//!
//! ## Crate layout
//!
//! ```text
//! src/
//!   lib.rs            crate root: module wiring + public re-exports
//!   error.rs          shared top-level error
//!   proposer/         OP Proposer ExEx (op-proposer port)
//!   withdrawals/      Withdrawal Cacher and Relayer ExEx (wip-1006)
//! ```

mod error;
mod proposer;
mod withdrawals;

pub use error::OpProposerError;
use proposer::{ContractError, DisputeGameFactory};
pub use proposer::{
    config::{ProposerCliArgs, ProposerConfig},
    db::{ProposerStore, StoredHead, StoredProposal},
    exex::{install_op_proposer_exex, op_proposer_exex},
    local_node::{ExExChainReader, ProviderBounds},
    provider::{L1Provider, L1ProviderConfig, SignerKind},
    service::{AdminRpcSettings, ProposerService},
    source::{
        Proposal, ProposalSource, ProposalSourceError, SyncStatus,
        local::{
            BlockMeta, ChainStatus, L2_TO_L1_MESSAGE_PASSER, LocalProposalSource,
            LocalStorageReader,
        },
    },
};
pub use withdrawals::{
    cacher::{ScanStats, prune_chain, scan_chain},
    config::{RelayerCliArgs, RelayerConfig, RelayerConfigError},
    exex::{CacherPrimitives, install_world_chain_relayer_exex, world_chain_relayer_exex},
    store::{StatusCounts, WithdrawalStore, WithdrawalStoreError},
    types::{
        MessagePassed, WithdrawalDecodeError, WithdrawalRecord, WithdrawalStatus,
        WithdrawalTransaction, message_slot, withdrawal_hash,
    },
};

pub type Result<T, E = OpProposerError> = std::result::Result<T, E>;
