use std::{collections::BTreeMap, path::Path};

use alloy_primitives::B256;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sp1_sdk::{CpuProver, HashableKey, Prover, ProvingKey, env::EnvProver};
use world_chain_proof_core::types::u32_to_u8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElfHash {
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElfHashes {
    #[serde(rename = "world-chain-range-ethereum")]
    pub range: ElfHash,
    #[serde(rename = "world-chain-aggregation")]
    pub aggregation: ElfHash,
}

/// Measurements that bind the embedded SP1 guest ELFs to the on-chain game configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedVkeyManifest {
    pub aggregation_vkey: B256,
    pub elfs: ElfHashes,
    pub range_vkey_commitment: B256,
}

/// Computes hashes and on-chain vkeys from the guest ELFs embedded in this binary.
pub async fn embedded_vkey_manifest() -> Result<EmbeddedVkeyManifest> {
    let range_elf = world_chain_proof_sp1_elfs::range_elf();
    let aggregation_elf = world_chain_proof_sp1_elfs::aggregation_elf();
    let range_sha256 = hex::encode(Sha256::digest(&*range_elf));
    let aggregation_sha256 = hex::encode(Sha256::digest(&*aggregation_elf));
    let client = EnvProver::Cpu(CpuProver::new().await);
    let range_pk = client
        .setup(range_elf)
        .await
        .map_err(|error| anyhow!("range setup failed: {error}"))?;
    let aggregation_pk = client
        .setup(aggregation_elf)
        .await
        .map_err(|error| anyhow!("aggregation setup failed: {error}"))?;

    Ok(EmbeddedVkeyManifest {
        aggregation_vkey: aggregation_pk
            .verifying_key()
            .bytes32()
            .parse()
            .map_err(|error| anyhow!("invalid aggregation vkey: {error}"))?,
        elfs: ElfHashes {
            range: ElfHash {
                sha256: range_sha256,
            },
            aggregation: ElfHash {
                sha256: aggregation_sha256,
            },
        },
        range_vkey_commitment: B256::from(u32_to_u8(range_pk.verifying_key().hash_u32())),
    })
}

/// Minimal view of `proof-releases.lock`: the release pointers plus
/// the SP1 measurement fields of each entry (other fields are ignored).
#[derive(Debug, Deserialize)]
struct Registry {
    latest_stable: String,
    latest_rc: String,
    releases: BTreeMap<String, RegistryEntry>,
}

#[derive(Debug, Deserialize)]
struct RegistryEntry {
    aggregation_vkey: B256,
    range_vkey_commitment: B256,
    aggregation_elf_sha256: String,
    range_elf_sha256: String,
}

/// The SP1 measurements of the registry's current release (`latest_rc`, else
/// `latest_stable`), for comparison against [`embedded_vkey_manifest`].
pub fn registry_vkey_manifest(path: &Path) -> Result<EmbeddedVkeyManifest> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| anyhow!("failed to read {}: {error}", path.display()))?;
    let registry: Registry = toml::from_str(&raw)
        .map_err(|error| anyhow!("failed to parse {}: {error}", path.display()))?;
    let current = if registry.latest_rc.is_empty() {
        &registry.latest_stable
    } else {
        &registry.latest_rc
    };
    if current.is_empty() {
        return Err(anyhow!(
            "{}: latest_rc and latest_stable are both empty — no current release",
            path.display()
        ));
    }
    let entry = registry
        .releases
        .get(current)
        .ok_or_else(|| anyhow!("{} has no [releases.\"{current}\"] entry", path.display()))?;
    Ok(EmbeddedVkeyManifest {
        aggregation_vkey: entry.aggregation_vkey,
        elfs: ElfHashes {
            range: ElfHash {
                sha256: entry.range_elf_sha256.clone(),
            },
            aggregation: ElfHash {
                sha256: entry.aggregation_elf_sha256.clone(),
            },
        },
        range_vkey_commitment: entry.range_vkey_commitment,
    })
}
