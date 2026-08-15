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
