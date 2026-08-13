use anyhow::{Result, bail};
use world_chain_proof_sp1_host::network_prover::SignerType;

pub mod deposit;
pub mod run;
pub mod succinct;

fn select_network_signer<'a>(
    private_key: Option<&'a str>,
    kms_key_id: Option<&'a str>,
) -> Result<(&'a str, SignerType)> {
    let private_key = private_key.filter(|value| !value.trim().is_empty());
    let kms_key_id = kms_key_id.filter(|value| !value.trim().is_empty());

    match (private_key, kms_key_id) {
        (Some(private_key), None) => Ok((private_key, SignerType::Local)),
        (None, Some(key_id)) => Ok((key_id, SignerType::AwsKms)),
        _ => bail!("configure exactly one SP1 private key or AWS KMS key ID"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_private_key_does_not_mask_kms_key() {
        let (secret, signer_type) =
            select_network_signer(Some("  "), Some("alias/prover")).unwrap();

        assert_eq!(secret, "alias/prover");
        assert!(matches!(signer_type, SignerType::AwsKms));
    }

    #[test]
    fn blank_kms_key_does_not_mask_private_key() {
        let (secret, signer_type) = select_network_signer(Some("0x1234"), Some("\t")).unwrap();

        assert_eq!(secret, "0x1234");
        assert!(matches!(signer_type, SignerType::Local));
    }

    #[test]
    fn rejects_missing_or_multiple_signers() {
        assert!(select_network_signer(None, None).is_err());
        assert!(select_network_signer(Some(" "), Some("\n")).is_err());
        assert!(select_network_signer(Some("0x1234"), Some("alias/prover")).is_err());
    }
}
