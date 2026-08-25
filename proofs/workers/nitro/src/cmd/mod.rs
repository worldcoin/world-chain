use anyhow::{Result, bail};
use world_chain_proof_nitro_register::SignerType;

pub mod common;
pub mod get_attestation;
pub mod register;
pub mod run;

fn select_registration_signer<'a>(
    private_key: Option<&'a str>,
    kms_key_id: Option<&'a str>,
    fallback_private_key: Option<&'a str>,
) -> Result<(&'a str, SignerType)> {
    let private_key = private_key.filter(|value| !value.trim().is_empty());
    let kms_key_id = kms_key_id.filter(|value| !value.trim().is_empty());
    let fallback_private_key = fallback_private_key.filter(|value| !value.trim().is_empty());

    // PRIVATE_KEY is a backwards-compatible fallback, not a second explicitly configured
    // signer. A dedicated KMS key ID must therefore take precedence over it.
    let private_key = if private_key.is_none() && kms_key_id.is_none() {
        fallback_private_key
    } else {
        private_key
    };

    match (private_key, kms_key_id) {
        (Some(private_key), None) => Ok((private_key, SignerType::Local)),
        (None, Some(key_id)) => Ok((key_id, SignerType::AwsKms)),
        _ => bail!("configure exactly one registration private key or AWS KMS key ID"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_private_key_does_not_mask_kms_key() {
        let (secret, signer_type) =
            select_registration_signer(Some("  "), Some("alias/register"), None).unwrap();

        assert_eq!(secret, "alias/register");
        assert_eq!(signer_type, SignerType::AwsKms);
    }

    #[test]
    fn blank_kms_key_does_not_mask_private_key() {
        let (secret, signer_type) =
            select_registration_signer(Some("0x1234"), Some("\t"), None).unwrap();

        assert_eq!(secret, "0x1234");
        assert_eq!(signer_type, SignerType::Local);
    }

    #[test]
    fn falls_back_to_private_key_only_when_no_dedicated_signer_is_set() {
        let (secret, signer_type) =
            select_registration_signer(None, None, Some("0xfallback")).unwrap();
        assert_eq!(secret, "0xfallback");
        assert_eq!(signer_type, SignerType::Local);

        let (secret, signer_type) =
            select_registration_signer(None, Some("alias/register"), Some("0xfallback")).unwrap();
        assert_eq!(secret, "alias/register");
        assert_eq!(signer_type, SignerType::AwsKms);
    }

    #[test]
    fn rejects_missing_or_multiple_signers() {
        assert!(select_registration_signer(None, None, None).is_err());
        assert!(select_registration_signer(Some(" "), Some("\n"), Some("\t")).is_err());
        assert!(select_registration_signer(Some("0x1234"), Some("alias/register"), None).is_err());
    }
}
