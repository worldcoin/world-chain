use crate::{
    Error, Result,
    config::{ConnectionConfig, MAX_PEM_BYTES, SigningTransport, read_bounded},
};
use bytes::Bytes;
use http::{Request, StatusCode, Version, header};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use metrics::{Counter, Gauge, Histogram};
use metrics_derive::Metrics;
use rustls::{
    ClientConfig, RootCertStore,
    client::Resumption,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{Mutex, Semaphore},
    task::JoinHandle,
    time::{Instant, timeout_at},
};
use tokio_rustls::TlsConnector;
use url::{Host, Position, Url};

pub const PROTOCOL: &str = "tee-kms-signer/1";
pub(crate) const SMALL_BODY: usize = 4096;
pub(crate) const TRANSACTION_BODY: usize = 1024 * 1024;
pub(crate) const PUBLICATION_RLP: usize = 16 * 1024 * 1024;
pub(crate) const AUTHORIZATION_RLP: usize = 256;
const MAX_REQUEST_DURATION: Duration = Duration::from_secs(3);
const ED25519_RESULT: usize = 64;
const BINARY_RESPONSE_HEADER: usize = size_of::<u64>() + size_of::<i32>();

impl SigningTransport {
    fn content_type(self) -> &'static str {
        match self {
            Self::JsonRpc => "application/json",
            Self::Binary => "application/octet-stream",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Method {
    Health,
    Grant,
    Publication,
    Transaction,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Self::Health => "health_status",
            Self::Grant => "teekms_signFlashblockAuthorizationV1",
            Self::Publication => "teekms_signFlashblockPublicationV1",
            Self::Transaction => "eth_signTransaction",
        }
    }
    fn request_limit(self) -> usize {
        match self {
            Self::Publication => PUBLICATION_RLP * 2 + SMALL_BODY,
            Self::Transaction => TRANSACTION_BODY,
            _ => SMALL_BODY,
        }
    }
    fn binary_request_limit(self) -> usize {
        match self {
            Self::Grant => 8 + 32 + 8 + 8 + 32,
            Self::Publication => 8 + 32 + 4 + PUBLICATION_RLP + AUTHORIZATION_RLP,
            Self::Transaction => TRANSACTION_BODY,
            Self::Health => 0,
        }
    }

    fn result_limit(self) -> usize {
        match self {
            Self::Grant | Self::Publication => ED25519_RESULT,
            Self::Transaction => TRANSACTION_BODY,
            Self::Health => 0,
        }
    }

    fn response_limit(self, transport: SigningTransport) -> usize {
        if transport == SigningTransport::Binary {
            return BINARY_RESPONSE_HEADER + self.result_limit();
        }
        match self {
            Self::Transaction => TRANSACTION_BODY + SMALL_BODY,
            _ => SMALL_BODY,
        }
    }

    fn path(self, transport: SigningTransport) -> &'static str {
        match (transport, self) {
            (SigningTransport::JsonRpc, _) | (_, Self::Health) => "/",
            (_, Self::Grant) => "/v1/sign/flashblock-authorization",
            (_, Self::Publication) => "/v1/sign/flashblock-publication",
            (_, Self::Transaction) => "/v1/sign/transaction",
        }
    }
}

#[derive(Metrics)]
#[metrics(scope = "worldchain.remote_signer")]
struct SignerMetrics {
    /// Admitted calls, including terminal denials and transport failures.
    requests: Counter,
    /// Calls that failed or whose futures were abandoned.
    failures: Counter,
    /// Currently admitted calls; bounded to one per client/key.
    inflight: Gauge,
    /// Complete client operation duration, including validation.
    duration_seconds: Histogram,
}

struct RequestMetrics<'a> {
    metrics: &'a SignerMetrics,
    started: std::time::Instant,
    success: bool,
}
impl Drop for RequestMetrics<'_> {
    fn drop(&mut self) {
        self.metrics.inflight.decrement(1.0);
        self.metrics
            .duration_seconds
            .record(self.started.elapsed().as_secs_f64());
        if !self.success {
            self.metrics.failures.increment(1);
        }
    }
}

struct Connection {
    sender: http1::SendRequest<Full<Bytes>>,
    driver: JoinHandle<()>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

pub(crate) struct Transport {
    endpoint: Url,
    wire: SigningTransport,
    tls: Arc<ClientConfig>,
    idle: Mutex<Option<Connection>>,
    operation: Arc<Semaphore>,
    connection: Arc<Semaphore>,
    next_id: AtomicU64,
    metrics: SignerMetrics,
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignerTransport")
            .field("endpoint", &self.endpoint.as_str())
            .field("wire", &self.wire)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct RpcRequest<'a, T> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<[&'a T; 1]>,
}

pub(crate) fn encode_request<T: Serialize>(
    method: Method,
    id: u64,
    params: Option<&T>,
) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(&RpcRequest {
        jsonrpc: "2.0",
        id,
        method: method.name(),
        params: params.map(|p| [p]),
    })
    .map_err(|_| Error::Config("cannot encode signer request"))?;
    if body.len() > method.request_limit() {
        return Err(Error::TooLarge("request body"));
    }
    Ok(body)
}

pub(crate) fn encode_binary_request(
    method: Method,
    id: u64,
    payload_len: usize,
    append: impl FnOnce(&mut Vec<u8>),
) -> Result<Vec<u8>> {
    let length = payload_len
        .checked_add(size_of::<u64>())
        .filter(|&length| length <= method.binary_request_limit())
        .ok_or(Error::TooLarge("request body"))?;
    let mut body = Vec::with_capacity(length);
    body.extend_from_slice(&id.to_be_bytes());
    append(&mut body);
    debug_assert_eq!(body.len(), length);
    Ok(body)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Response {
    Success(Success),
    Failure(Failure),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Success {
    jsonrpc: String,
    id: u64,
    result: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    jsonrpc: String,
    id: u64,
    error: RpcError,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcError {
    code: i64,
    #[serde(rename = "message")]
    _message: String,
    data: ErrorData,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorData {
    reason: String,
    retryable: bool,
}

fn decode_response(bytes: &[u8], expected_id: u64) -> Result<String> {
    let response: Response =
        serde_json::from_slice(bytes).map_err(|_| Error::InvalidResponse("invalid V1 envelope"))?;
    let (version, id) = match &response {
        Response::Success(s) => (&s.jsonrpc, s.id),
        Response::Failure(s) => (&s.jsonrpc, s.id),
    };
    if version != "2.0" || id != expected_id {
        return Err(Error::InvalidResponse("version or request ID mismatch"));
    }
    match response {
        Response::Success(s) => Ok(s.result),
        Response::Failure(s) => {
            let reason = error_reason(s.error.code)?;
            if s.error.data.retryable || s.error.data.reason != reason {
                return Err(Error::InvalidResponse("invalid V1 error semantics"));
            }
            Err(Error::Remote {
                code: s.error.code,
                reason,
            })
        }
    }
}

fn error_reason(code: i64) -> Result<&'static str> {
    match code {
        -32700 => Ok("parse_error"),
        -32600 => Ok("invalid_request"),
        -32601 => Ok("method_not_found"),
        -32602 => Ok("invalid_params"),
        -32603 => Ok("internal_error"),
        -32001 => Ok("denied"),
        -32002 => Ok("authorization_unavailable"),
        -32003 => Ok("busy"),
        -32004 => Ok("dependency_unavailable"),
        -32005 => Ok("clock_unavailable"),
        -32006 => Ok("deadline"),
        _ => Err(Error::InvalidResponse("unknown V1 error code")),
    }
}

fn decode_binary_response(bytes: &[u8], expected_id: u64) -> Result<&[u8]> {
    let (id, rest) = bytes
        .split_first_chunk::<8>()
        .ok_or(Error::InvalidResponse("incomplete binary response header"))?;
    let (status, result) = rest
        .split_first_chunk::<4>()
        .ok_or(Error::InvalidResponse("incomplete binary response header"))?;
    if u64::from_be_bytes(*id) != expected_id {
        return Err(Error::InvalidResponse("request ID mismatch"));
    }
    let code = i64::from(i32::from_be_bytes(*status));
    if code == 0 {
        return Ok(result);
    }
    if !result.is_empty() {
        return Err(Error::InvalidResponse("unexpected binary error payload"));
    }
    Err(Error::Remote {
        code,
        reason: error_reason(code)?,
    })
}

impl Transport {
    pub(crate) fn new(config: ConnectionConfig, purpose: &'static str) -> Result<Self> {
        config.validate()?;
        let ca = read_bounded(&config.tls.ca, MAX_PEM_BYTES)?;
        let cert = read_bounded(&config.tls.certificate, MAX_PEM_BYTES)?;
        let key = read_bounded(&config.tls.private_key, MAX_PEM_BYTES)?;
        let mut roots = RootCertStore::empty();
        for cert in CertificateDer::pem_slice_iter(&ca) {
            roots
                .add(cert.map_err(|_| Error::Config("invalid server CA PEM"))?)
                .map_err(|_| Error::Config("invalid server CA certificate"))?;
        }
        if roots.is_empty() {
            return Err(Error::Config("explicit server CA is required"));
        }
        let certificates = CertificateDer::pem_slice_iter(&cert)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::Config("invalid client certificate PEM"))?;
        if certificates.is_empty() {
            return Err(Error::Config("client certificate is required"));
        }
        let private_key = PrivateKeyDer::from_pem_slice(&key)
            .map_err(|_| Error::Config("invalid client private key PEM"))?;
        let mut tls =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(|_| Error::Config("TLS 1.3 is unavailable"))?
                .with_root_certificates(roots)
                .with_client_auth_cert(certificates, private_key)
                .map_err(|_| Error::Config("client certificate/key do not match"))?;
        tls.resumption = Resumption::disabled();
        tls.enable_early_data = false;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            endpoint: config.endpoint,
            wire: config.transport,
            tls: Arc::new(tls),
            idle: Mutex::new(None),
            operation: Arc::new(Semaphore::new(1)),
            connection: Arc::new(Semaphore::new(1)),
            next_id: AtomicU64::new(1),
            metrics: SignerMetrics::new_with_labels(&[("purpose", purpose)]),
        })
    }

    pub(crate) async fn health(&self, deadline: Instant) -> Result<()> {
        self.call(
            Method::Health,
            SigningTransport::JsonRpc,
            deadline,
            |id| encode_request::<()>(Method::Health, id, None),
            |bytes, id| {
                if decode_response(bytes, id)? == PROTOCOL {
                    Ok(())
                } else {
                    Err(Error::InvalidResponse("protocol identity mismatch"))
                }
            },
        )
        .await
    }

    pub(crate) async fn sign<R>(
        &self,
        method: Method,
        deadline: Instant,
        encode: impl FnOnce(SigningTransport, u64) -> Result<Vec<u8>> + Send,
        verify: impl FnOnce(&[u8]) -> Result<R> + Send,
    ) -> Result<R> {
        self.call(
            method,
            self.wire,
            deadline,
            |id| encode(self.wire, id),
            |bytes, id| match self.wire {
                SigningTransport::JsonRpc => {
                    let value = decode_response(bytes, id)?;
                    verify(&decode_hex(&value, method.result_limit())?)
                }
                SigningTransport::Binary => verify(decode_binary_response(bytes, id)?),
            },
        )
        .await
    }

    /// Bounded caller-side handover wait; no RPC is sent or retried here.
    pub(crate) async fn wait_until_idle(&self, deadline: Instant) -> Result<()> {
        let deadline = deadline.min(Instant::now() + MAX_REQUEST_DURATION);
        let permit = timeout_at(deadline, self.operation.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Cancelled)?;
        drop(permit);
        Ok(())
    }

    async fn call<R>(
        &self,
        method: Method,
        wire: SigningTransport,
        deadline: Instant,
        encode: impl FnOnce(u64) -> Result<Vec<u8>> + Send,
        decode: impl FnOnce(&[u8], u64) -> Result<R> + Send,
    ) -> Result<R> {
        let _permit = self
            .operation
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let deadline = deadline.min(Instant::now() + MAX_REQUEST_DURATION);
        if deadline <= Instant::now() {
            return Err(Error::Deadline);
        }
        let id = self
            .next_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Error::InvalidResponse("request ID space exhausted"))?;
        self.metrics.requests.increment(1);
        self.metrics.inflight.increment(1.0);
        let mut metrics = RequestMetrics {
            metrics: &self.metrics,
            started: std::time::Instant::now(),
            success: false,
        };
        let body = encode(id)?;
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        let result = timeout_at(deadline, async {
            let stored = self.idle.lock().await.take();
            let mut connection = match stored {
                Some(c) if !c.sender.is_closed() => c,
                _ => self.connect().await?,
            };
            let authority = &self.endpoint[Position::BeforeHost..Position::AfterPort];
            let request = Request::builder()
                .method("POST")
                .uri(method.path(wire))
                .header(header::HOST, authority)
                .header(header::CONTENT_TYPE, wire.content_type())
                .header(header::CONTENT_LENGTH, body.len())
                .body(Full::new(Bytes::from(body)))
                .map_err(|_| Error::Config("cannot construct signer HTTP request"))?;
            let response = connection
                .sender
                .send_request(request)
                .await
                .map_err(|e| Error::transport("HTTP request", e))?;
            let bytes = read_response(response, method, wire).await?;
            let verified = decode(&bytes, id);
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            if verified.is_ok() || matches!(verified, Err(Error::Remote { .. })) {
                *self.idle.lock().await = Some(connection);
            }
            verified
        })
        .await
        .map_err(|_| Error::Deadline)?;
        metrics.success = result.is_ok();
        result
    }

    async fn connect(&self) -> Result<Connection> {
        let permit = self
            .connection
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Cancelled)?;
        let host = match self
            .endpoint
            .host()
            .ok_or(Error::Config("missing endpoint host"))?
        {
            Host::Domain(s) => s.to_owned(),
            Host::Ipv4(ip) => ip.to_string(),
            Host::Ipv6(ip) => ip.to_string(),
        };
        let port = self
            .endpoint
            .port_or_known_default()
            .ok_or(Error::Config("missing endpoint port"))?;
        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| Error::transport("TCP connect", e))?;
        stream
            .set_nodelay(true)
            .map_err(|e| Error::transport("TCP configuration", e))?;
        let name =
            ServerName::try_from(host).map_err(|_| Error::Config("invalid TLS server name"))?;
        let stream = TlsConnector::from(self.tls.clone())
            .connect(name, stream)
            .await
            .map_err(|e| Error::transport("mTLS handshake", e))?;
        let (sender, driver) = http1::Builder::new()
            .max_headers(32)
            .max_buf_size(16 * 1024)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|e| Error::transport("HTTP handshake", e))?;
        let driver = tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = driver.await {
                tracing::debug!(target: "worldchain::remote_signer", %error, "signer HTTP connection closed");
            }
        });
        Ok(Connection { sender, driver })
    }
}

async fn read_response(
    response: http::Response<hyper::body::Incoming>,
    method: Method,
    wire: SigningTransport,
) -> Result<Vec<u8>> {
    if response.version() != Version::HTTP_11 {
        return Err(Error::InvalidResponse("HTTP/1.1 required"));
    }
    if response.status() != StatusCode::OK {
        return Err(Error::Http(response.status().as_u16()));
    }
    if response.headers().contains_key(header::TRANSFER_ENCODING)
        || response.headers().contains_key(header::CONTENT_ENCODING)
        || response
            .headers()
            .get_all(header::CONTENT_LENGTH)
            .iter()
            .count()
            != 1
    {
        return Err(Error::InvalidResponse("unsupported HTTP framing/encoding"));
    }
    let mut content_types = response.headers().get_all(header::CONTENT_TYPE).iter();
    if content_types.next().and_then(|value| value.to_str().ok()) != Some(wire.content_type())
        || content_types.next().is_some()
    {
        return Err(Error::InvalidResponse("unexpected content type"));
    }
    let length = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(Error::InvalidResponse("invalid content length"))?;
    if length > method.response_limit(wire) {
        return Err(Error::TooLarge("response body"));
    }
    let mut bytes = Vec::with_capacity(length);
    let mut response = response.into_body();
    while let Some(frame) = response.frame().await {
        let frame = frame.map_err(|e| Error::transport("HTTP response body", e))?;
        let data = frame
            .into_data()
            .map_err(|_| Error::InvalidResponse("HTTP trailers are not supported"))?;
        if data.len() > length.saturating_sub(bytes.len()) {
            return Err(Error::TooLarge("response body"));
        }
        bytes.extend_from_slice(&data);
    }
    if bytes.len() != length {
        return Err(Error::InvalidResponse("incomplete HTTP response"));
    }
    Ok(bytes)
}

pub(crate) fn decode_hex(value: &str, limit: usize) -> Result<Vec<u8>> {
    let value = value
        .strip_prefix("0x")
        .ok_or(Error::InvalidResponse("missing hex prefix"))?;
    if value.len() % 2 != 0 || value.len() > limit * 2 {
        return Err(Error::InvalidResponse("invalid hex length"));
    }
    hex::decode(value).map_err(|_| Error::InvalidResponse("invalid hex bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_response_requires_complete_header_matching_id_and_exact_error_frame() {
        let id = 0x1122_aabb_3344_5566u64;
        let mut body = id.to_be_bytes().to_vec();
        body.extend_from_slice(&0i32.to_be_bytes());
        for length in 0..BINARY_RESPONSE_HEADER {
            assert!(decode_binary_response(&body[..length], id).is_err());
        }
        body.extend_from_slice(&[0xa5; 64]);
        assert_eq!(decode_binary_response(&body, id), Ok(&[0xa5; 64][..]));
        assert!(decode_binary_response(&body, id + 1).is_err());

        body.truncate(size_of::<u64>());
        body.extend_from_slice(&(-32002i32).to_be_bytes());
        assert_eq!(
            decode_binary_response(&body, id),
            Err(Error::Remote {
                code: -32002,
                reason: "authorization_unavailable"
            })
        );
        body.push(0);
        assert!(matches!(
            decode_binary_response(&body, id),
            Err(Error::InvalidResponse(_))
        ));
        body.truncate(size_of::<u64>());
        body.extend_from_slice(&1i32.to_be_bytes());
        assert!(matches!(
            decode_binary_response(&body, id),
            Err(Error::InvalidResponse(_))
        ));
    }

    #[test]
    fn oversized_binary_request_is_rejected_before_encoding() {
        let method = Method::Transaction;
        for payload_len in [TRANSACTION_BODY - 7, usize::MAX] {
            assert_eq!(
                encode_binary_request(method, 1, payload_len, |_| {
                    panic!("oversized request must fail before allocation")
                }),
                Err(Error::TooLarge("request body"))
            );
        }
    }

    #[tokio::test]
    async fn binary_http_response_rejects_wrong_status_mime_and_incomplete_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let valid = "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 12\r\n\r\n";
        for (head, bytes, expected) in [
            (valid.to_owned(), 12, Ok(vec![0; 12])),
            (valid.to_owned(), 11, Err("truncated")),
            (
                valid.replace("application/octet-stream", "application/json"),
                12,
                Err("mime"),
            ),
            (
                valid.replace("200 OK", "503 Service Unavailable"),
                12,
                Err("status"),
            ),
        ] {
            let (client, mut peer) = tokio::io::duplex(512);
            let (mut sender, driver) = http1::handshake::<_, Full<Bytes>>(TokioIo::new(client))
                .await
                .expect("HTTP/1.1 client");
            let driver = tokio::spawn(driver);
            let server = tokio::spawn(async move {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(peer.read_u8().await.expect("request bytes"));
                    assert!(request.len() < 512);
                }
                peer.write_all(head.as_bytes())
                    .await
                    .expect("response header");
                peer.write_all(&[0; 12][..bytes])
                    .await
                    .expect("response bytes");
                peer.shutdown().await.expect("end response");
            });
            let response = sender
                .send_request(Request::new(Full::new(Bytes::new())))
                .await
                .expect("response headers");
            let got = read_response(response, Method::Grant, SigningTransport::Binary).await;
            match expected {
                Ok(body) => assert_eq!(got, Ok(body)),
                Err("truncated") => assert!(matches!(
                    got,
                    Err(Error::Transport {
                        operation: "HTTP response body",
                        ..
                    })
                )),
                Err("mime") => {
                    assert_eq!(got, Err(Error::InvalidResponse("unexpected content type")))
                }
                Err("status") => assert_eq!(got, Err(Error::Http(503))),
                Err(other) => panic!("unexpected case {other}"),
            }
            server.await.expect("loopback HTTP peer");
            driver.abort();
        }
    }
    #[test]
    fn publication_json_body_accepts_exact_cap_and_rejects_cap_plus_one() {
        let cap = 32 * 1024 * 1024 + 4096;
        assert_eq!(Method::Publication.request_limit(), cap);
        let overhead = encode_request(Method::Publication, 17, Some(&""))
            .expect("empty envelope")
            .len();
        let value = "a".repeat(cap - overhead);
        assert_eq!(
            encode_request(Method::Publication, 17, Some(&value))
                .expect("exact cap")
                .len(),
            cap
        );
        let value = value + "a";
        assert_eq!(
            encode_request(Method::Publication, 17, Some(&value)),
            Err(Error::TooLarge("request body"))
        );
    }
    #[test]
    fn response_ids_and_envelopes_are_strict_and_lossless() {
        let id = 9_007_199_254_740_993;
        let body = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"ok"}}"#);
        assert_eq!(decode_response(body.as_bytes(), id), Ok("ok".into()));
        assert!(decode_response(body.as_bytes(), id - 1).is_err());
        for body in [
            r#"{"jsonrpc":"2.0","id":1,"id":1,"result":"ok"}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":"ok","extra":0}"#,
            r#"{"jsonrpc":"2.0","id":"1","result":"ok"}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":null}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":"ok","error":null}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":"ok"} {}"#,
        ] {
            assert!(decode_response(body.as_bytes(), 1).is_err(), "{body}");
        }
    }

    #[test]
    fn v1_errors_never_enable_retry_or_accept_unknown_reasons() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"denied","data":{"reason":"denied","retryable":false}}}"#;
        assert_eq!(
            decode_response(body.as_bytes(), 1),
            Err(Error::Remote {
                code: -32001,
                reason: "denied"
            })
        );
        assert!(matches!(
            decode_response(body.replace("false", "true").as_bytes(), 1),
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            decode_response(
                body.replace("\"reason\":\"denied\"", "\"reason\":\"busy\"")
                    .as_bytes(),
                1
            ),
            Err(Error::InvalidResponse(_))
        ));
    }
}
