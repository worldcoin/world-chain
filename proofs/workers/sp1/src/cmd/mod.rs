use world_chain_proof_sp1_host::network_prover::SignerType;

pub mod deposit;
pub mod run;
pub mod succinct;
pub mod vkeys;

fn select_network_signer<'a>(
    private_key: Option<&'a str>,
    kms_key_id: Option<&'a str>,
) -> (&'a str, SignerType) {
    if let Some(private_key) = private_key {
        (private_key, SignerType::Local)
    } else {
        (
            kms_key_id.expect("SP1 signer is required by clap"),
            SignerType::AwsKms,
        )
    }
}
