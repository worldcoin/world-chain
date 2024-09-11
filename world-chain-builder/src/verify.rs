use eyre::eyre::bail;
use semaphore::protocol::{verify_proof, Proof};
use semaphore::Field;
use tracing::error;

pub struct SemaphoreProof {
    pub root: Field,
    pub nullifier_hash: Field,
    pub signal_hash: Field,
    pub external_nullifier_hash: Field,
    pub proof: Proof,
}

pub async fn verify_semaphore_proof(proof: SemaphoreProof) -> eyre::Result<()> {
    // TODO: think about how we want to verify the roots
    let checked = verify_proof(
        proof.root,
        proof.nullifier_hash,
        proof.signal_hash,
        proof.external_nullifier_hash,
        &proof.proof,
        30,
    );

    match checked {
        Ok(true) => return Ok(()),
        Ok(false) => bail!("invalid proof"),
        Err(err) => {
            error!(?err, "verify_proof failed with error");
            bail!("invalid proof")
        }
    }
}
