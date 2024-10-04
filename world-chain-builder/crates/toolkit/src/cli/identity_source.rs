use std::path::PathBuf;

use bytes::BytesMut;
use clap::Args;
use semaphore::identity::Identity;

use super::utils::bytes_parse_hex;

#[derive(Debug, Clone, Args)]
pub struct IdentitySource {
    #[clap(
        short = 'I',
        long,
        conflicts_with = "identity_file",
        required_unless_present = "identity_file",
        value_parser = bytes_parse_hex
    )]
    pub identity: Option<BytesMut>,

    #[clap(
        long,
        conflicts_with = "identity",
        required_unless_present = "identity"
    )]
    pub identity_file: Option<PathBuf>,
}

impl IdentitySource {
    pub fn load(&self) -> Identity {
        if let Some(mut identity) = self.identity.clone() {
            return Identity::from_secret(identity.as_mut(), None);
        }

        if let Some(identity_file) = &self.identity_file {
            let mut identity = std::fs::read(identity_file).unwrap();
            return Identity::from_secret(identity.as_mut(), None);
        }

        unreachable!()
    }
}
