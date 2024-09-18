use alloy_rlp::{Decodable, Encodable};
use semaphore::Field;
use serde::{Deserialize, Serialize};

const LEN: usize = 256;

pub type ProofBytes = [u8; LEN];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof(pub semaphore::protocol::Proof);

impl Decodable for Proof {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let bytes = ProofBytes::decode(buf)?;
        if bytes.len() != LEN {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }
        let fields: [[u8; 32]; 8] = bytemuck::cast(bytes);
        let a = (fields[0].into(), fields[1].into());
        let b = (
            [fields[2].into(), fields[3].into()],
            [fields[4].into(), fields[5].into()],
        );
        let c = (fields[6].into(), fields[7].into());
        let proof = semaphore::protocol::Proof(a, b, c);
        Ok(Proof(proof))
    }
}

impl Encodable for Proof {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let fields: [[u8; 32]; 8] = [
            self.0 .0 .0.into(),
            self.0 .0 .1.into(),
            self.0 .1 .0[0].into(),
            self.0 .1 .0[1].into(),
            self.0 .1 .1[0].into(),
            self.0 .1 .1[1].into(),
            self.0 .2 .0.into(),
            self.0 .2 .1.into(),
        ];

        let bytes: ProofBytes = bytemuck::cast(fields);
        bytes.encode(out)
    }
}

// #[derive(Clone, Debug, RlpEncodable, RlpDecodable, PartialEq, Eq)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemaphoreProof {
    pub root: Field,
    pub signal_hash: Field,
    pub nullifier_hash: Field,
    pub external_nullifier_hash: Field,
    pub proof: Proof,
    pub external_nullifier: String,
}
