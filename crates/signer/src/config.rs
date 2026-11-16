use crate::{Error, Result};
use alloy_primitives::{Address, B256};
use serde::Deserialize;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use url::Url;
use zeroize::Zeroizing;

const MAX_CONFIG_BYTES: usize = 64 * 1024;
pub(crate) const MAX_PEM_BYTES: usize = 256 * 1024;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TlsFiles {
    pub ca: PathBuf,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionConfig {
    pub endpoint: Url,
    pub tls: TlsFiles,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ed25519Config {
    pub connection: ConnectionConfig,
    pub key_id: B256,
    pub expected_public_key: B256,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PbhConfig {
    pub connection: ConnectionConfig,
    pub expected_address: Address,
}

pub(crate) fn read_bounded(path: &Path, limit: usize) -> Result<Zeroizing<Vec<u8>>> {
    let file = File::open(path).map_err(|_| Error::Config("cannot open configuration/PEM file"))?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(limit + 1));
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Config("cannot read configuration/PEM file"))?;
    if bytes.len() > limit {
        return Err(Error::Config("configuration/PEM file is too large"));
    }
    Ok(bytes)
}

impl ConnectionConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        let url = &self.endpoint;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Config(
                "endpoint must be HTTPS at / without user info, query or fragment",
            ));
        }
        Ok(())
    }

    fn relative_to(&mut self, base: &Path) {
        for path in [
            &mut self.tls.ca,
            &mut self.tls.certificate,
            &mut self.tls.private_key,
        ] {
            if path.is_relative() {
                *path = base.join(&path);
            }
        }
    }
}

impl Ed25519Config {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = read_bounded(path, MAX_CONFIG_BYTES)?;
        let mut value: Self = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Config("invalid Ed25519 signer JSON"))?;
        value
            .connection
            .relative_to(path.parent().unwrap_or_else(|| Path::new(".")));
        value.connection.validate()?;
        Ok(value)
    }
}

impl PbhConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = read_bounded(path, MAX_CONFIG_BYTES)?;
        let mut value: Self =
            serde_json::from_slice(&bytes).map_err(|_| Error::Config("invalid PBH signer JSON"))?;
        value
            .connection
            .relative_to(path.parent().unwrap_or_else(|| Path::new(".")));
        value.connection.validate()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(endpoint: &str) -> serde_json::Value {
        serde_json::json!({
            "connection": {"endpoint":endpoint,"tls":{"ca":"ca.pem","certificate":"client.pem","privateKey":"client.key"}},
            "expectedAddress":"0x1111111111111111111111111111111111111111"
        })
    }

    #[test]
    fn configuration_resolves_credentials_relative_to_its_own_directory() {
        let directory = tempfile::tempdir().expect("temporary configuration");
        let path = directory.path().join("pbh.json");
        std::fs::write(&path, config("https://signer.example/").to_string())
            .expect("configuration");
        let loaded = PbhConfig::load(&path).expect("valid configuration");
        assert_eq!(loaded.connection.tls.ca, directory.path().join("ca.pem"));
        assert_eq!(
            loaded.connection.tls.certificate,
            directory.path().join("client.pem")
        );
        assert_eq!(
            loaded.connection.tls.private_key,
            directory.path().join("client.key")
        );
    }

    #[test]
    fn ambiguous_configuration_and_non_v1_endpoints_fail_closed() {
        for endpoint in [
            "http://signer.example/",
            "https://user@signer.example/",
            "https://signer.example/rpc",
            "https://signer.example/?key=public",
            "https://signer.example/#fragment",
        ] {
            let config: PbhConfig =
                serde_json::from_value(config(endpoint)).expect("valid URL syntax");
            assert!(config.connection.validate().is_err());
        }
        let valid = config("https://signer.example/").to_string();
        let unknown = valid.replacen("{", "{\"unexpected\":true,", 1);
        let duplicate = valid.replacen(
            "{",
            "{\"expectedAddress\":\"0x2222222222222222222222222222222222222222\",",
            1,
        );
        assert!(serde_json::from_str::<PbhConfig>(&unknown).is_err());
        assert!(serde_json::from_str::<PbhConfig>(&duplicate).is_err());
        let directory = tempfile::tempdir().expect("temporary configuration");
        let path = directory.path().join("large.json");
        std::fs::write(&path, vec![b' '; MAX_CONFIG_BYTES + 1]).expect("oversized configuration");
        assert!(matches!(
            PbhConfig::load(&path),
            Err(Error::Config("configuration/PEM file is too large"))
        ));
    }
}
