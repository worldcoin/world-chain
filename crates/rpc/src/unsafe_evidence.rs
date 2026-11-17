//! Allowlisted forwarding to one paired op-node; evidence is never reconstructed.

use alloy_primitives::B256;
use jsonrpsee::{
    RpcModule,
    core::{RegisterMethodError, RpcResult},
    types::{ErrorObjectOwned, Id, Response, ResponsePayload},
};
use reqwest::{Client, Url};
use serde_json::{json, value::RawValue};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

const CAPABILITIES: &str = "optimism_unsafeBlockEvidenceCapabilities";
const GET_EVIDENCE: &str = "optimism_getUnsafeBlockEvidence";
const DEADLINE: Duration = Duration::from_secs(5);
/// Includes the upstream JSON-RPC envelope, with headroom above a 10 MiB hex frame.
pub const MAX_RESPONSE_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct UnsafeBlockEvidenceRpc {
    client: Client,
    endpoint: Url,
    in_flight: Arc<Semaphore>,
    deadline: Duration,
}

impl UnsafeBlockEvidenceRpc {
    pub fn into_rpc(self) -> Result<RpcModule<Self>, RegisterMethodError> {
        let mut module = RpcModule::new(self);
        module.register_async_method(CAPABILITIES, |params, rpc, _| async move {
            if params.as_str().is_some() {
                params.parse::<[serde_json::Value; 0]>()?;
            }
            rpc.unsafe_block_evidence_capabilities().await
        })?;
        module.register_async_method(GET_EVIDENCE, |params, rpc, _| async move {
            let hash = params.one::<B256>()?;
            rpc.get_unsafe_block_evidence(hash).await
        })?;
        Ok(module)
    }

    pub fn new(endpoint: &str) -> eyre::Result<Self> {
        let endpoint = Url::parse(endpoint)
            .map_err(|_| eyre::eyre::eyre!("invalid unsafe-block evidence op-node URL"))?;
        eyre::eyre::ensure!(
            matches!(endpoint.scheme(), "http" | "https")
                && endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "unsafe-block evidence requires an HTTP(S) op-node URL without credentials, query or fragment"
        );
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .timeout(DEADLINE)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .pool_max_idle_per_host(2)
            .build()?;
        Ok(Self {
            client,
            endpoint,
            in_flight: Arc::new(Semaphore::new(2)),
            deadline: DEADLINE,
        })
    }

    async fn forward(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> RpcResult<Box<RawValue>> {
        let _permit = self
            .in_flight
            .try_acquire()
            .map_err(|_| unavailable(method, "busy"))?;
        let started = std::time::Instant::now();
        // Own the request future: timeout or caller cancellation drops the upstream I/O.
        let result = tokio::time::timeout(self.deadline, self.request(method, params))
            .await
            .map_err(|_| unavailable(method, "timeout"));
        metrics::histogram!("rpc.unsafe_evidence.duration_seconds", "method" => method)
            .record(started.elapsed().as_secs_f64());
        result?
    }

    async fn request(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> RpcResult<Box<RawValue>> {
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .map_err(|error| {
                unavailable(
                    method,
                    if error.is_timeout() {
                        "timeout"
                    } else {
                        "transport"
                    },
                )
            })?;
        if !response.status().is_success() {
            return Err(unavailable(method, "httpStatus"));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(unavailable(method, "responseTooLarge"));
        }
        let capacity = response
            .content_length()
            .unwrap_or(MAX_RESPONSE_BYTES as u64) as usize;
        let mut bytes = Vec::with_capacity(capacity);
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            unavailable(
                method,
                if error.is_timeout() {
                    "timeout"
                } else {
                    "transport"
                },
            )
        })? {
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                return Err(unavailable(method, "responseTooLarge"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: Response<'_, Box<RawValue>> =
            serde_json::from_slice(&bytes).map_err(|_| unavailable(method, "invalidResponse"))?;
        if response.jsonrpc.is_none() || response.id != Id::Number(1) {
            return Err(unavailable(method, "invalidResponse"));
        }
        metrics::histogram!("rpc.unsafe_evidence.response_bytes", "method" => method)
            .record(bytes.len() as f64);
        match response.payload {
            ResponsePayload::Success(result) => {
                if method == CAPABILITIES {
                    serde_json::from_str::<Vec<String>>(result.get())
                        .map_err(|_| unavailable(method, "invalidResponse"))?;
                } else {
                    #[derive(serde::Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct EvidenceIdentity {
                        version: u64,
                        block_hash: B256,
                    }
                    let identity: EvidenceIdentity = serde_json::from_str(result.get())
                        .map_err(|_| unavailable(method, "invalidResponse"))?;
                    if identity.version == 0 || params != json!([identity.block_hash]) {
                        return Err(unavailable(method, "invalidResponse"));
                    }
                }
                metrics::counter!("rpc.unsafe_evidence.requests_total", "method" => method, "outcome" => "success").increment(1);
                Ok(result.into_owned())
            }
            ResponsePayload::Error(error) => {
                metrics::counter!("rpc.unsafe_evidence.requests_total", "method" => method, "outcome" => "upstreamRpcError").increment(1);
                Err(error.into_owned())
            }
        }
    }
}

fn unavailable(method: &'static str, reason: &'static str) -> ErrorObjectOwned {
    metrics::counter!("rpc.unsafe_evidence.requests_total", "method" => method, "outcome" => reason).increment(1);
    ErrorObjectOwned::owned(
        -32081,
        "Unsafe-block evidence upstream unavailable",
        Some(json!({"reason": reason})),
    )
}

impl UnsafeBlockEvidenceRpc {
    pub async fn unsafe_block_evidence_capabilities(&self) -> RpcResult<Box<RawValue>> {
        self.forward(CAPABILITIES, json!([])).await
    }

    pub async fn get_unsafe_block_evidence(&self, hash: B256) -> RpcResult<Box<RawValue>> {
        self.forward(GET_EVIDENCE, json!([hash])).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::{
        RpcModule,
        server::{ServerBuilder, ServerHandle},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };

    async fn serve(module: RpcModule<()>) -> (String, ServerHandle) {
        let server = ServerBuilder::default()
            .set_config(
                jsonrpsee::server::ServerConfig::builder()
                    .max_response_body_size((MAX_RESPONSE_BYTES + 1024) as u32)
                    .build(),
            )
            .build("127.0.0.1:0")
            .await
            .expect("bind RPC");
        let endpoint = format!("http://{}", server.local_addr().expect("local address"));
        (endpoint, server.start(module))
    }

    #[tokio::test]
    async fn maximum_result_and_original_errors_survive_both_http_hops() {
        let hash = B256::repeat_byte(1);
        let payload = "aB".repeat(10 * 1024 * 1024 - 65);
        let signature = "cD".repeat(65);
        let original = format!(
            r#"{{ "version":1, "chainId":"480", "blockHash":"{hash}", "blockVersion":3, "payload":"0x{payload}", "signature":"0x{signature}", "extension":{{"future":true}} }}"#
        );
        let retained = original.clone();
        let mut upstream = RpcModule::new(());
        upstream
            .register_method(CAPABILITIES, |_, _, _| {
                vec!["optimism_unsafe_block_evidence_v1"]
            })
            .expect("capabilities");
        upstream
            .register_method(
                GET_EVIDENCE,
                move |params, _, _| -> RpcResult<Box<RawValue>> {
                    let requested: B256 = params.one()?;
                    if requested != hash {
                        return Err(ErrorObjectOwned::owned(
                            -32080,
                            "unsafe-block signing evidence unavailable",
                            Some(json!({"blockHash": requested, "reason": "expired"})),
                        ));
                    }
                    Ok(RawValue::from_string(retained.clone()).expect("synthetic JSON"))
                },
            )
            .expect("lookup");
        let (endpoint, upstream) = serve(upstream).await;
        let proxy = UnsafeBlockEvidenceRpc::new(&endpoint).expect("proxy");
        let module = proxy
            .into_rpc()
            .expect("registered methods")
            .remove_context();
        let mut names: Vec<_> = module.method_names().collect();
        names.sort_unstable();
        assert_eq!(names, [GET_EVIDENCE, CAPABILITIES]);
        let (endpoint, proxy) = serve(module).await;
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client");
        for (method, params) in [
            (CAPABILITIES, json!([])),
            (GET_EVIDENCE, json!([hash])),
            (GET_EVIDENCE, json!([B256::ZERO])),
            (GET_EVIDENCE, json!(["latest"])),
            (GET_EVIDENCE, json!([hash, "latest"])),
            (GET_EVIDENCE, json!({"hash": hash})),
            (CAPABILITIES, json!([hash])),
            ("admin_sequencerActive", json!([])),
        ] {
            let bytes = client
                .post(&endpoint)
                .json(&json!({"jsonrpc":"2.0","id":7,"method":method,"params":params}))
                .send()
                .await
                .expect("HTTP response")
                .bytes()
                .await
                .expect("body");
            let response: Response<'_, Box<RawValue>> =
                serde_json::from_slice(&bytes).expect("JSON-RPC response");
            assert_eq!(response.id, Id::Number(7));
            match (method, params.as_array().and_then(|p| p.first())) {
                (CAPABILITIES, _) if params == json!([]) => {
                    let response = jsonrpsee::types::response::Success::try_from(response)
                        .expect("capabilities result");
                    assert_eq!(
                        response.result.get(),
                        r#"["optimism_unsafe_block_evidence_v1"]"#
                    );
                }
                (GET_EVIDENCE, Some(value)) if *value == json!(hash) && params == json!([hash]) => {
                    let response = jsonrpsee::types::response::Success::try_from(response)
                        .expect("evidence result");
                    assert_eq!(response.result.get(), original);
                    assert!(bytes.len() > 20 * 1024 * 1024);
                }
                _ => {
                    let ResponsePayload::Error(error) = response.payload else {
                        panic!("expected error")
                    };
                    if method == "admin_sequencerActive" {
                        assert_eq!(error.code(), -32601);
                    } else if params != json!([B256::ZERO]) {
                        assert_eq!(error.code(), -32602);
                    } else {
                        assert_eq!(error.code(), -32080);
                        let data: serde_json::Value =
                            serde_json::from_str(error.data().expect("error data").get())
                                .expect("data JSON");
                        assert_eq!(data, json!({"blockHash": B256::ZERO, "reason": "expired"}));
                    }
                }
            }
        }
        proxy.stop().expect("stop proxy");
        upstream.stop().expect("stop upstream");
    }

    #[tokio::test]
    async fn deadline_cancellation_and_admission_never_retry() {
        let requests = Arc::new(AtomicUsize::new(0));
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut module = RpcModule::new(());
        let count = requests.clone();
        module
            .register_async_method(GET_EVIDENCE, move |_, _, _| {
                count.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).expect("request observed");
                std::future::pending::<RpcResult<Box<RawValue>>>()
            })
            .expect("slow lookup");
        let (endpoint, server) = serve(module).await;
        let rpc = UnsafeBlockEvidenceRpc::new(&endpoint).expect("proxy");
        let first = tokio::spawn({
            let rpc = rpc.clone();
            async move { rpc.get_unsafe_block_evidence(B256::ZERO).await }
        });
        let second = tokio::spawn({
            let rpc = rpc.clone();
            async move { rpc.get_unsafe_block_evidence(B256::ZERO).await }
        });
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(2), started_rx.recv())
                .await
                .expect("request deadline")
                .expect("request arrived");
        }
        let busy = rpc
            .get_unsafe_block_evidence(B256::ZERO)
            .await
            .expect_err("load shed");
        assert_eq!(busy.code(), -32081);
        assert_eq!(rpc.in_flight.available_permits(), 0);
        first.abort();
        second.abort();
        assert!(first.await.expect_err("cancelled").is_cancelled());
        assert!(second.await.expect_err("cancelled").is_cancelled());
        assert_eq!(rpc.in_flight.available_permits(), 2);
        let mut rpc = rpc;
        rpc.deadline = Duration::from_millis(30);
        let error = rpc
            .get_unsafe_block_evidence(B256::ZERO)
            .await
            .expect_err("deadline");
        assert_eq!(error.code(), -32081);
        assert_eq!(
            error.data().expect("error data").get(),
            r#"{"reason":"timeout"}"#
        );
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        assert_eq!(rpc.in_flight.available_permits(), 2);
        server.stop().expect("stop upstream");
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) {
        let mut reader = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).await.expect("header") > 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().expect("request length");
            }
        }
        assert!(length < 4096);
        reader
            .read_exact(&mut vec![0; length])
            .await
            .expect("complete body");
    }

    #[tokio::test]
    async fn dropping_forwarding_work_closes_the_upstream_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let rpc = UnsafeBlockEvidenceRpc::new(&format!(
            "http://{}",
            listener.local_addr().expect("address")
        ))
        .expect("proxy");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("connection");
            read_request(&mut stream).await;
            started_tx.send(()).expect("started");
            assert_eq!(stream.read(&mut [0]).await.expect("connection close"), 0);
        });
        let request = tokio::spawn({
            let rpc = rpc.clone();
            async move { rpc.get_unsafe_block_evidence(B256::ZERO).await }
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .expect("request deadline")
            .expect("request started");
        request.abort();
        assert!(request.await.expect_err("cancelled").is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), upstream)
            .await
            .expect("close deadline")
            .expect("closed upstream");
        assert_eq!(rpc.in_flight.available_permits(), 2);
    }

    async fn raw_upstream(
        response: String,
    ) -> (UnsafeBlockEvidenceRpc, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let rpc = UnsafeBlockEvidenceRpc::new(&format!(
            "http://{}",
            listener.local_addr().expect("address")
        ))
        .expect("proxy");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("connection");
            read_request(&mut stream).await;
            // Rejection may close the connection before the entire synthetic response is sent.
            let _ = stream.write_all(response.as_bytes()).await;
        });
        (rpc, task)
    }

    #[tokio::test]
    async fn mixed_versions_malformed_envelopes_redirects_and_oversize_fail_explicitly() {
        for body in [
            r#"{"jsonrpc":"2.0","id":2,"result":[]}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":[],"error":{"code":-32080,"message":"missing"}}"#,
            r#"{"id":1,"result":[]}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":null}"#,
            "not JSON",
        ] {
            let (rpc, task) = raw_upstream(format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ))
            .await;
            let error = rpc
                .unsafe_block_evidence_capabilities()
                .await
                .expect_err("invalid envelope");
            assert_eq!(error.code(), -32081);
            task.await.expect("fixture");
        }
        let body =
            json!({"jsonrpc":"2.0","id":1,"result":{"version":1,"blockHash":B256::repeat_byte(1)}})
                .to_string();
        let (rpc, task) = raw_upstream(format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
        .await;
        assert_eq!(
            rpc.get_unsafe_block_evidence(B256::ZERO)
                .await
                .expect_err("wrong block identity")
                .code(),
            -32081
        );
        task.await.expect("fixture");
        let oversized = " ".repeat(MAX_RESPONSE_BYTES + 1);
        let (rpc, task) = raw_upstream(format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{oversized}\r\n0\r\n\r\n",
            oversized.len()
        ))
        .await;
        let error = rpc
            .get_unsafe_block_evidence(B256::ZERO)
            .await
            .expect_err("chunked response limit");
        assert_eq!(error.code(), -32081);
        assert_eq!(
            error.data().expect("reason").get(),
            r#"{"reason":"responseTooLarge"}"#
        );
        task.await.expect("fixture");
        for response in [
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_RESPONSE_BYTES + 1
            ),
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1\r\nContent-Length: 0\r\n\r\n"
                .to_owned(),
            "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".to_owned(),
        ] {
            let (rpc, task) = raw_upstream(response).await;
            assert_eq!(
                rpc.get_unsafe_block_evidence(B256::ZERO)
                    .await
                    .expect_err("unavailable")
                    .code(),
                -32081
            );
            task.await.expect("fixture");
        }
        let (endpoint, server) = serve(RpcModule::new(())).await;
        let rpc = UnsafeBlockEvidenceRpc::new(&endpoint).expect("proxy");
        assert_eq!(
            rpc.unsafe_block_evidence_capabilities()
                .await
                .expect_err("old op-node")
                .code(),
            -32601
        );
        server.stop().expect("stop upstream");
        for endpoint in [
            "ftp://127.0.0.1",
            "http://name:secret@127.0.0.1",
            "http://127.0.0.1?token=secret",
            "http://127.0.0.1#fragment",
        ] {
            assert!(UnsafeBlockEvidenceRpc::new(endpoint).is_err());
        }
    }
}
