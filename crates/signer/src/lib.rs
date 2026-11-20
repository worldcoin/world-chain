//! Purpose-specific clients for the pinned signer V1 protocol.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod bridge;
pub mod config;
mod ed25519;
mod error;
mod pbh;
mod transport;
mod wallet;

pub use ed25519::{GrantSigner, PublicationSigner, validate_publication};
pub use error::{Error, Result};
pub use pbh::PbhSigner;
pub use transport::PROTOCOL;
pub use wallet::{LocalPbhSigner, PbhWallet};
