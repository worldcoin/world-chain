use async_trait::async_trait;
use aws_credential_types::provider::ProvideCredentials;
use eyre::eyre::{Context, OptionExt, bail};
use serde::{Serialize, de::DeserializeOwned};

/// Byte-oriented blob store used for withdrawal handoff JSON.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Read the blob at `key`.
    async fn get(&self, key: &str) -> eyre::Result<Vec<u8>>;
    /// Write `bytes` to `key`.
    async fn put(&self, key: &str, bytes: &[u8]) -> eyre::Result<()>;
    /// Return whether `key` exists.
    async fn exists(&self, key: &str) -> eyre::Result<bool>;
    /// Delete `key`. Missing keys are ignored.
    async fn delete(&self, key: &str) -> eyre::Result<()>;
}

/// Filesystem-backed store. The key is treated as a filesystem path.
pub struct LocalStore;

#[async_trait]
impl BlobStore for LocalStore {
    async fn get(&self, key: &str) -> eyre::Result<Vec<u8>> {
        Ok(std::fs::read(key)?)
    }

    async fn put(&self, key: &str, bytes: &[u8]) -> eyre::Result<()> {
        Ok(std::fs::write(key, bytes)?)
    }

    async fn exists(&self, key: &str) -> eyre::Result<bool> {
        Ok(std::path::Path::new(key).exists())
    }

    async fn delete(&self, key: &str) -> eyre::Result<()> {
        match std::fs::remove_file(key) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

/// S3-backed store for a single bucket.
pub struct S3Store {
    client: aws_sdk_s3::Client,
    bucket: String,
}

#[async_trait]
impl BlobStore for S3Store {
    async fn get(&self, key: &str) -> eyre::Result<Vec<u8>> {
        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .wrap_err_with(|| format!("failed to get s3://{}/{}", self.bucket, key))?;
        let bytes = object
            .body
            .collect()
            .await
            .wrap_err_with(|| format!("failed to read s3://{}/{} body", self.bucket, key))?
            .into_bytes()
            .to_vec();
        Ok(bytes)
    }

    async fn put(&self, key: &str, bytes: &[u8]) -> eyre::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type("application/json")
            .body(bytes.to_vec().into())
            .send()
            .await
            .wrap_err_with(|| format!("failed to put s3://{}/{}", self.bucket, key))?;
        Ok(())
    }

    async fn exists(&self, key: &str) -> eyre::Result<bool> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(err) => {
                if err.as_service_error().is_some_and(|e| e.is_not_found()) {
                    Ok(false)
                } else {
                    Err(err)
                        .wrap_err_with(|| format!("failed to head s3://{}/{}", self.bucket, key))
                }
            }
        }
    }

    async fn delete(&self, key: &str) -> eyre::Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .wrap_err_with(|| format!("failed to delete s3://{}/{}", self.bucket, key))?;
        Ok(())
    }
}

/// Open a location URI and return the store plus the key within that store.
///
/// - Local path: any string that does not start with `s3://`
/// - S3: `s3://bucket/key`
pub async fn open(location: &str) -> eyre::Result<(Box<dyn BlobStore>, String)> {
    if let Some(rest) = location.strip_prefix("s3://") {
        let (bucket, key) = rest
            .split_once('/')
            .ok_or_eyre("s3 location must be s3://bucket/key")?;
        if bucket.is_empty() {
            bail!("s3 location missing bucket: {location}");
        }
        if key.is_empty() {
            bail!("s3 location missing key: {location}");
        }
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        if config.region().is_none() {
            bail!(
                "AWS region not configured; set AWS_REGION or AWS_DEFAULT_REGION \
                 (needed for s3:// locations)"
            );
        }
        // Resolve credentials early so missing AWS_PROFILE / expired SSO is obvious,
        // instead of a later opaque "dispatch failure" from IMDS fallback.
        if let Some(provider) = config.credentials_provider() {
            provider.provide_credentials().await.wrap_err(
                "AWS credentials unavailable; set AWS_PROFILE to an SSO profile \
                 (e.g. tfh-crypto-dev-poweruseraccess) and run \
                 `aws sso login --profile <profile>`",
            )?;
        }
        let store = S3Store {
            client: aws_sdk_s3::Client::new(&config),
            bucket: bucket.to_string(),
        };
        return Ok((Box::new(store), key.to_string()));
    }

    Ok((Box::new(LocalStore), location.to_string()))
}

/// Read and deserialize JSON from a local path or `s3://bucket/key`.
pub async fn read_json<T: DeserializeOwned>(location: &str) -> eyre::Result<T> {
    let (store, key) = open(location).await?;
    Ok(serde_json::from_slice(&store.get(&key).await?)?)
}

/// Serialize and write JSON to a local path or `s3://bucket/key`.
pub async fn write_json<T: Serialize>(location: &str, value: &T) -> eyre::Result<()> {
    let (store, key) = open(location).await?;
    store.put(&key, &serde_json::to_vec_pretty(value)?).await
}

/// Return whether a local path or `s3://bucket/key` exists.
pub async fn exists(location: &str) -> eyre::Result<bool> {
    let (store, key) = open(location).await?;
    store.exists(&key).await
}

/// Delete a local path or `s3://bucket/key`. Missing objects are ignored.
pub async fn delete(location: &str) -> eyre::Result<()> {
    let (store, key) = open(location).await?;
    store.delete(&key).await
}
