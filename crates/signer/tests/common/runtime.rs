use alloy_primitives::B256;
use ed25519_dalek::SigningKey;
use serde_json::Value;
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::Instant;
use world_chain_signer::config::{
    ConnectionConfig, Ed25519Config, PbhConfig, SigningTransport, TlsFiles,
};

pub const SIGNER_REVISION: &str = "47f267856dfb08660e1c9bbee123c9e9f93397d6";

pub struct Harness {
    child: Child,
    directory: tempfile::TempDir,
    metadata: Value,
    transport: SigningTransport,
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Only this test's child is stopped; no operational process is discovered or signalled.
        match self.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => eprintln!("disposable harness status failed: {error}"),
        }
        if let Err(error) = self.child.kill() {
            eprintln!("disposable harness cleanup signal failed: {error}");
        }
        if let Err(error) = self.child.wait() {
            eprintln!("disposable harness cleanup wait failed: {error}");
        }
    }
}

impl Harness {
    pub async fn start() -> Self {
        let executable = std::env::var_os("WORLD_CHAIN_SIGNER_HARNESS_BINARY")
            .expect("build the pinned disposable signer harness first");
        let revision = std::env::var("WORLD_CHAIN_SIGNER_HARNESS_REVISION")
            .expect("record the reviewed harness revision");
        assert_eq!(
            revision, SIGNER_REVISION,
            "use the reviewed merged signer source"
        );
        let transport = match std::env::var("WORLD_CHAIN_SIGNER_HARNESS_TRANSPORT").as_deref() {
            Ok("binary") => SigningTransport::Binary,
            Ok("json-rpc") | Err(std::env::VarError::NotPresent) => SigningTransport::JsonRpc,
            _ => panic!("set harness transport to binary or json-rpc"),
        };
        let directory = tempfile::tempdir().expect("private test directory");
        let run = directory.path().join("runtime");
        let mut child = Command::new(executable)
            .arg(&run)
            .arg("300")
            .env("SIMULATION_MODE", "TFH_ONLY")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start disposable harness");
        let stdout = child.stdout.take().expect("metadata pipe");
        let mut harness = Self {
            child,
            directory,
            metadata: Value::Null,
            transport,
        };
        let ready = tokio::task::spawn_blocking(move || {
            let mut line = String::new();
            BufReader::new(stdout)
                .read_line(&mut line)
                .expect("harness ready record");
            line
        });
        let line = tokio::time::timeout(Duration::from_secs(15), ready)
            .await
            .expect("bounded harness startup")
            .expect("ready reader");
        harness.metadata = serde_json::from_str(&line).expect("nonsecret harness metadata");
        assert_eq!(
            harness.metadata["pid"].as_u64(),
            Some(u64::from(harness.child.id()))
        );
        assert_eq!(harness.metadata["simulation"], true);
        assert_eq!(harness.metadata["hostClockUnqualified"], true);
        assert_eq!(harness.metadata["publicTestScalar"], 1);
        let endpoint: url::Url = harness.metadata["endpoint"]
            .as_str()
            .expect("endpoint")
            .parse()
            .expect("test URL");
        assert_eq!(endpoint.host_str(), Some("127.0.0.1"));
        assert_eq!(endpoint.scheme(), "https");
        assert_eq!(
            harness.metadata["flashblocks"]["authorizerPublicKey"]
                .as_str()
                .expect("public key")
                .parse::<B256>()
                .expect("key"),
            B256::from(SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes())
        );
        assert_eq!(
            harness.metadata["flashblocks"]["publisherPublicKey"]
                .as_str()
                .expect("public key")
                .parse::<B256>()
                .expect("key"),
            B256::from(SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes())
        );
        assert_eq!(
            harness.metadata["transactions"]["pbh"]["address"],
            "0x2b5ad5c4795c026514f8317c7a215e218dccd6cf"
        );
        harness
    }

    fn run_directory(&self) -> PathBuf {
        self.directory.path().join("runtime")
    }
    pub fn connection(&self, role: &str) -> ConnectionConfig {
        let directory = self.run_directory();
        ConnectionConfig {
            endpoint: self.metadata["endpoint"]
                .as_str()
                .expect("endpoint")
                .parse()
                .expect("URL"),
            transport: self.transport,
            tls: TlsFiles {
                ca: directory.join("simulation-ca.pem"),
                certificate: directory.join(format!("simulation-{role}.pem")),
                private_key: directory.join(format!("simulation-{role}.key")),
            },
        }
    }
    pub fn ed25519(&self, authorizer: bool, credential_role: &str) -> Ed25519Config {
        let (key, seed) = if authorizer {
            ("authorizerKeyId", 7)
        } else {
            ("publisherKeyId", 8)
        };
        Ed25519Config {
            connection: self.connection(credential_role),
            key_id: self.metadata["flashblocks"][key]
                .as_str()
                .expect("test key selector")
                .parse()
                .expect("key ID"),
            expected_public_key: B256::from(
                SigningKey::from_bytes(&[seed; 32])
                    .verifying_key()
                    .to_bytes(),
            ),
        }
    }
    pub fn pbh(&self, credential_role: &str) -> PbhConfig {
        PbhConfig {
            connection: self.connection(credential_role),
            expected_address: "0x2b5ad5c4795c026514f8317c7a215e218dccd6cf"
                .parse()
                .expect("public scalar-two address"),
        }
    }
    pub fn stop_renewal(&self) {
        let status = Command::new("/bin/kill")
            .arg("-USR1")
            .arg(self.child.id().to_string())
            .status()
            .expect("signal owned harness");
        assert!(status.success());
    }
}

pub fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}
