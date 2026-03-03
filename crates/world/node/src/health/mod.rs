use std::{
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use async_trait::async_trait;
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Serialize;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

pub mod checks;
pub mod config;

pub use config::{CheckConfig, HealthConfig, ProbeConfig};

pub struct CheckResult {
    pub healthy: bool,
    pub detail: Option<String>,
}

#[async_trait]
pub trait HealthCheck: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    async fn check(&self) -> CheckResult;
}

#[derive(Clone, Serialize)]
pub struct CheckOutcome {
    pub name: &'static str,
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct ProbeResult {
    pub healthy: bool,
    pub checks: Vec<CheckOutcome>,
}

impl Default for ProbeResult {
    fn default() -> Self {
        Self { healthy: true, checks: vec![] }
    }
}

#[derive(Clone)]
pub struct Probe {
    checks: Vec<Arc<dyn HealthCheck>>,
    pub interval: Duration,
    result: Arc<RwLock<ProbeResult>>,
}

impl Default for Probe {
    fn default() -> Self {
        Self::new(vec![], Duration::from_secs(30))
    }
}

impl Probe {
    pub fn new(checks: Vec<Arc<dyn HealthCheck>>, interval: Duration) -> Self {
        Self { checks, interval, result: Arc::new(RwLock::new(ProbeResult::default())) }
    }

    pub async fn evaluate(&self) -> ProbeResult {
        let mut outcomes = Vec::with_capacity(self.checks.len());
        let mut healthy = true;

        for check in &self.checks {
            let result = check.check().await;
            if !result.healthy {
                warn!(
                    target: "world_chain::health",
                    check = check.name(),
                    detail = ?result.detail,
                    "health check failed"
                );
                healthy = false;
            }
            outcomes.push(CheckOutcome {
                name: check.name(),
                healthy: result.healthy,
                detail: result.detail,
            });
        }

        ProbeResult { healthy, checks: outcomes }
    }

    async fn init(&self) {
        let r = self.evaluate().await;
        *self.result.write().expect("probe result lock poisoned") = r;
    }

    pub fn last_result(&self) -> ProbeResult {
        self.result.read().expect("probe result lock poisoned").clone()
    }

    pub async fn run_loop(self) {
        loop {
            tokio::time::sleep(self.interval).await;
            let r = self.evaluate().await;
            *self.result.write().expect("probe result lock poisoned") = r;
        }
    }
}

#[derive(Clone)]
pub struct HealthServer {
    pub startup: Probe,
    pub readiness: Probe,
    pub liveness: Probe,
    addr: SocketAddr,
}

impl HealthServer {
    pub fn new(startup: Probe, readiness: Probe, liveness: Probe, addr: SocketAddr) -> Self {
        Self { startup, readiness, liveness, addr }
    }

    pub async fn serve(self) {
        let listener = match TcpListener::bind(self.addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(target: "world_chain::health", addr = %self.addr, "Failed to bind health server: {e}");
                return;
            }
        };
        info!(target: "world_chain::health", addr = %self.addr, "Health server listening");

        // Run initial evaluation before accepting connections.
        self.startup.init().await;
        self.readiness.init().await;
        self.liveness.init().await;

        // Spawn background evaluation loops.
        tokio::spawn(self.startup.clone().run_loop());
        tokio::spawn(self.readiness.clone().run_loop());
        tokio::spawn(self.liveness.clone().run_loop());

        let state = Arc::new(self);
        let app = Router::new()
            .route("/startup", get(startup_handler))
            .route("/ready", get(readiness_handler))
            .route("/live", get(liveness_handler))
            .with_state(state);

        if let Err(e) = axum::serve(listener, app).await {
            error!(target: "world_chain::health", "Health server error: {e}");
        }
    }
}

async fn startup_handler(State(s): State<Arc<HealthServer>>) -> (StatusCode, Json<ProbeResult>) {
    probe_response(s.startup.last_result())
}

async fn readiness_handler(State(s): State<Arc<HealthServer>>) -> (StatusCode, Json<ProbeResult>) {
    probe_response(s.readiness.last_result())
}

async fn liveness_handler(State(s): State<Arc<HealthServer>>) -> (StatusCode, Json<ProbeResult>) {
    probe_response(s.liveness.last_result())
}

fn probe_response(result: ProbeResult) -> (StatusCode, Json<ProbeResult>) {
    let status =
        if result.healthy { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (status, Json(result))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use async_trait::async_trait;

    use super::*;

    struct AlwaysHealthy;

    #[async_trait]
    impl HealthCheck for AlwaysHealthy {
        fn name(&self) -> &'static str {
            "always_healthy"
        }

        async fn check(&self) -> CheckResult {
            CheckResult { healthy: true, detail: None }
        }
    }

    struct AlwaysUnhealthy;

    #[async_trait]
    impl HealthCheck for AlwaysUnhealthy {
        fn name(&self) -> &'static str {
            "always_unhealthy"
        }

        async fn check(&self) -> CheckResult {
            CheckResult { healthy: false, detail: Some("broken".to_string()) }
        }
    }

    fn interval() -> Duration {
        Duration::from_secs(30)
    }

    #[tokio::test]
    async fn probe_all_healthy() {
        let probe = Probe::new(
            vec![Arc::new(AlwaysHealthy), Arc::new(AlwaysHealthy)],
            interval(),
        );
        let result = probe.evaluate().await;
        assert!(result.healthy);
        assert_eq!(result.checks.len(), 2);
        assert!(result.checks.iter().all(|c| c.healthy));
    }

    #[tokio::test]
    async fn probe_one_unhealthy_marks_probe_unhealthy() {
        let probe = Probe::new(
            vec![Arc::new(AlwaysHealthy), Arc::new(AlwaysUnhealthy)],
            interval(),
        );
        let result = probe.evaluate().await;
        assert!(!result.healthy);
        assert_eq!(result.checks.len(), 2);
    }

    #[tokio::test]
    async fn probe_all_checks_run_even_after_failure() {
        let probe = Probe::new(
            vec![Arc::new(AlwaysUnhealthy), Arc::new(AlwaysHealthy)],
            interval(),
        );
        let result = probe.evaluate().await;
        assert!(!result.healthy);
        assert_eq!(result.checks.len(), 2);
        assert_eq!(result.checks[0].name, "always_unhealthy");
        assert!(!result.checks[0].healthy);
        assert_eq!(result.checks[1].name, "always_healthy");
        assert!(result.checks[1].healthy);
    }

    #[tokio::test]
    async fn probe_empty_is_healthy() {
        let probe = Probe::default();
        let result = probe.evaluate().await;
        assert!(result.healthy);
        assert!(result.checks.is_empty());
    }
}
