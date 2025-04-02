use rand::RngCore;
use semaphore_rs::{hash::Hash, identity::Identity, Field};
use serde::{Deserialize, Serialize};

use super::GenerateArgs;

#[derive(Debug, Serialize, Deserialize)]
pub struct SerializableIdentity {
    pub nullifier: Field,
    pub trapdoor: Field,
}

impl From<&Identity> for SerializableIdentity {
    fn from(identity: &Identity) -> Self {
        Self {
            nullifier: identity.nullifier,
            trapdoor: identity.trapdoor,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct InsertCommitmentRequest {
    pub identity_commitment: Hash,
}

/// Generate Test identities, and inserts them into the signup sequencer.
pub async fn generate_identities(args: GenerateArgs) -> eyre::Result<()> {
    let identies: Vec<_> = (0..args.count)
        .map(|_| {
            let mut rng = rand::thread_rng();

            let mut secret = [0_u8; 64];
            rng.fill_bytes(&mut secret);

            let identity = Identity::from_secret(&mut secret, None);

            Identity {
                nullifier: identity.nullifier,
                trapdoor: identity.trapdoor,
            }
        })
        .collect();

    serde_json::to_writer(
        std::fs::File::create(args.path)?,
        &identies
            .iter()
            .map(SerializableIdentity::from)
            .collect::<Vec<_>>(),
    )?;

    let client = reqwest::Client::new();
    futures::future::try_join_all(identies.iter().map(|identity| {
        insert_identity(
            &client,
            &args.sequencer_url,
            &args.username,
            &args.password,
            identity.clone(),
        )
    }))
    .await?;
    Ok(())
}

async fn insert_identity(
    client: &reqwest::Client,
    sequencer_url: &str,
    username: &str,
    password: &str,
    identity: Identity,
) -> eyre::Result<()> {
    let response = client
        .post(format!("{}/insertIdentity", sequencer_url))
        .basic_auth(username, Some(password))
        .json(&InsertCommitmentRequest {
            identity_commitment: identity.commitment().into(),
        })
        .send()
        .await?;

    let _ = response.error_for_status()?;
    Ok(())
}
