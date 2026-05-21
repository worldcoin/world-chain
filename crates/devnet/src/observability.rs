use std::time::Duration;

use eyre::eyre::{Context, Result, bail};
use serde_json::Value;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tracing::info;

use crate::{
    DevnetPortMode,
    component::{ContainerImage, DevnetComponent, DevnetComponentKind, DevnetComponentStatus},
    process_logs::{ProcessLogTarget, container_log_consumer},
};

const PROMETHEUS_PORT: u16 = 9090;
const GRAFANA_PORT: u16 = 3000;
const GRAFANA_PROMETHEUS_UID: &str = "devnet-prometheus";
const GRAFANA_DASHBOARD_DIR: &str = "/var/lib/grafana/dashboards/world-chain";
const FLASHBLOCKS_PAYLOAD_BUILDER_DASHBOARD: &str =
    include_str!("../../../pkg/devnet/grafana/dashboards/flashblocks-payload-builder.json");
const FLASHBLOCKS_VALIDATION_PIPELINE_DASHBOARD: &str =
    include_str!("../../../pkg/devnet/grafana/dashboards/flashblocks-validation-pipeline.json");
const FLASHBLOCKS_P2P_DASHBOARD: &str =
    include_str!("../../../pkg/devnet/grafana/dashboards/flashblocks-p2p.json");
const WORLD_CHAIN_DASHBOARDS: [(&str, &str); 3] = [
    (
        "flashblocks-payload-builder.json",
        FLASHBLOCKS_PAYLOAD_BUILDER_DASHBOARD,
    ),
    (
        "flashblocks-validation-pipeline.json",
        FLASHBLOCKS_VALIDATION_PIPELINE_DASHBOARD,
    ),
    ("flashblocks-p2p.json", FLASHBLOCKS_P2P_DASHBOARD),
];

/// Metrics scrape target registered with the devnet Prometheus instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricsTarget {
    /// Prometheus job name.
    pub job: String,
    /// Host/port target as seen from the Prometheus container.
    pub target: String,
}

impl MetricsTarget {
    /// Build a scrape target.
    pub fn new(job: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            job: job.into(),
            target: target.into(),
        }
    }
}

/// Configuration for the optional Prometheus/Grafana devnet stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservabilityConfig {
    /// Whether to start observability containers.
    pub enabled: bool,
    /// Prometheus image.
    pub prometheus_image: ContainerImage,
    /// Grafana image.
    pub grafana_image: ContainerImage,
    /// Optional stable Prometheus host port.
    pub stable_prometheus_port: Option<u16>,
    /// Optional stable Grafana host port.
    pub stable_grafana_port: Option<u16>,
    /// Prometheus scrape interval.
    pub scrape_interval_secs: u64,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            prometheus_image: ContainerImage::new("prom/prometheus", "v3.1.0"),
            grafana_image: ContainerImage::new("grafana/grafana", "12.2"),
            stable_prometheus_port: None,
            stable_grafana_port: None,
            scrape_interval_secs: 5,
        }
    }
}

impl ObservabilityConfig {
    /// Enabled config with default image choices.
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Apply dynamic or stable host-port policy.
    pub fn with_port_mode(mut self, port_mode: DevnetPortMode) -> Self {
        if port_mode == DevnetPortMode::Stable {
            self.stable_prometheus_port = Some(PROMETHEUS_PORT);
            self.stable_grafana_port = Some(GRAFANA_PORT);
        } else {
            self.stable_prometheus_port = None;
            self.stable_grafana_port = None;
        }
        self
    }
}

/// Lifecycle-owned observability containers.
#[derive(Debug)]
pub struct ObservabilityStack {
    prometheus_url: String,
    grafana_url: String,
    prometheus_image: ContainerImage,
    grafana_image: ContainerImage,
    _prometheus: ContainerAsync<GenericImage>,
    _grafana: ContainerAsync<GenericImage>,
}

impl ObservabilityStack {
    /// Start Prometheus and Grafana through testcontainers.
    pub async fn start(
        config: ObservabilityConfig,
        targets: Vec<MetricsTarget>,
    ) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }

        info!(
            prometheus_image = %config.prometheus_image,
            grafana_image = %config.grafana_image,
            scrape_targets = targets.len(),
            prometheus_stable_port = ?config.stable_prometheus_port,
            grafana_stable_port = ?config.stable_grafana_port,
            "starting devnet observability containers"
        );

        let prometheus_config = render_prometheus_config(config.scrape_interval_secs, &targets);
        let prometheus_image = GenericImage::new(
            config.prometheus_image.repository.clone(),
            config.prometheus_image.tag.clone(),
        )
        .with_wait_for(WaitFor::seconds(1))
        .with_exposed_port(PROMETHEUS_PORT.tcp())
        .with_log_consumer(container_log_consumer(
            "prometheus",
            ProcessLogTarget::Prometheus,
        ))
        .with_copy_to(
            "/etc/prometheus/prometheus.yml",
            prometheus_config.into_bytes(),
        )
        .with_cmd([
            "--config.file=/etc/prometheus/prometheus.yml".to_string(),
            "--storage.tsdb.path=/prometheus".to_string(),
            "--web.console.libraries=/usr/share/prometheus/console_libraries".to_string(),
            "--web.console.templates=/usr/share/prometheus/consoles".to_string(),
            format!("--web.listen-address=0.0.0.0:{PROMETHEUS_PORT}"),
        ]);

        let mut prometheus_request = prometheus_image.with_startup_timeout(Duration::from_secs(90));
        if let Some(host_port) = config.stable_prometheus_port {
            prometheus_request =
                prometheus_request.with_mapped_port(host_port, PROMETHEUS_PORT.tcp());
        }

        let prometheus = prometheus_request
            .start()
            .await
            .wrap_err("failed to start Prometheus container")?;
        let prometheus_host = prometheus.get_host().await?;
        let prometheus_port = prometheus.get_host_port_ipv4(PROMETHEUS_PORT.tcp()).await?;
        let prometheus_url = format!("http://{prometheus_host}:{prometheus_port}");
        let grafana_prometheus_url = format!("http://host.docker.internal:{prometheus_port}");
        wait_for_http_service(
            "Prometheus",
            &prometheus,
            &format!("{prometheus_url}/-/ready"),
            Duration::from_secs(90),
        )
        .await?;

        let grafana_image = GenericImage::new(
            config.grafana_image.repository.clone(),
            config.grafana_image.tag.clone(),
        )
        .with_wait_for(WaitFor::seconds(1))
        .with_exposed_port(GRAFANA_PORT.tcp())
        .with_log_consumer(container_log_consumer("grafana", ProcessLogTarget::Grafana))
        .with_env_var("GF_AUTH_ANONYMOUS_ENABLED", "true")
        .with_env_var("GF_AUTH_ANONYMOUS_ORG_ROLE", "Admin")
        .with_env_var("GF_AUTH_DISABLE_LOGIN_FORM", "true")
        .with_copy_to(
            "/etc/grafana/provisioning/datasources/world-chain-devnet.yml",
            render_grafana_datasource(&grafana_prometheus_url).into_bytes(),
        )
        .with_copy_to(
            "/etc/grafana/provisioning/dashboards/world-chain-devnet.yml",
            render_grafana_dashboard_provider().into_bytes(),
        );

        let mut grafana_image = grafana_image;
        for (filename, dashboard) in WORLD_CHAIN_DASHBOARDS {
            let dashboard = provision_grafana_dashboard(dashboard)?;
            grafana_image = grafana_image
                .with_copy_to(format!("{GRAFANA_DASHBOARD_DIR}/{filename}"), dashboard);
        }

        let mut grafana_request = grafana_image.with_startup_timeout(Duration::from_secs(90));
        if let Some(host_port) = config.stable_grafana_port {
            grafana_request = grafana_request.with_mapped_port(host_port, GRAFANA_PORT.tcp());
        }

        let grafana = grafana_request
            .start()
            .await
            .wrap_err("failed to start Grafana container")?;
        let grafana_host = grafana.get_host().await?;
        let grafana_port = grafana.get_host_port_ipv4(GRAFANA_PORT.tcp()).await?;
        let grafana_url = format!("http://{grafana_host}:{grafana_port}");
        wait_for_http_service(
            "Grafana",
            &grafana,
            &format!("{grafana_url}/api/health"),
            Duration::from_secs(90),
        )
        .await?;

        info!(
            %prometheus_url,
            %grafana_url,
            dashboards = WORLD_CHAIN_DASHBOARDS.len(),
            "devnet observability ready"
        );

        Ok(Some(Self {
            prometheus_url,
            grafana_url,
            prometheus_image: config.prometheus_image,
            grafana_image: config.grafana_image,
            _prometheus: prometheus,
            _grafana: grafana,
        }))
    }

    /// Prometheus UI URL.
    pub fn prometheus_url(&self) -> &str {
        &self.prometheus_url
    }

    /// Grafana UI URL.
    pub fn grafana_url(&self) -> &str {
        &self.grafana_url
    }

    /// Component manifest entries for the running observability stack.
    pub fn components(&self) -> [DevnetComponent; 2] {
        [
            DevnetComponent::new(
                "prometheus",
                DevnetComponentKind::Prometheus,
                DevnetComponentStatus::Running,
            )
            .with_image(self.prometheus_image.clone())
            .with_endpoint("http", self.prometheus_url.clone()),
            DevnetComponent::new(
                "grafana",
                DevnetComponentKind::Grafana,
                DevnetComponentStatus::Running,
            )
            .with_image(self.grafana_image.clone())
            .with_endpoint("http", self.grafana_url.clone()),
        ]
    }
}

async fn wait_for_http_service(
    name: &str,
    container: &ContainerAsync<GenericImage>,
    url: &str,
    timeout: Duration,
) -> Result<()> {
    let started_at = tokio::time::Instant::now();
    let mut last_error = None;

    while started_at.elapsed() < timeout {
        match reqwest::get(url).await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                last_error = Some(format!("HTTP {}", response.status()));
            }
            Err(err) => {
                last_error = Some(err.to_string());
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let logs = tail_text(&container_logs(container).await, 40);
    let last_error = last_error.unwrap_or_else(|| "no HTTP response".to_string());
    bail!("{name} did not become ready at {url}: {last_error}\ncontainer log tail:\n{logs}");
}

async fn container_logs(container: &ContainerAsync<GenericImage>) -> String {
    let stdout = container.stdout_to_vec().await.unwrap_or_default();
    let stderr = container.stderr_to_vec().await.unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    format!("stdout:\n{stdout}\nstderr:\n{stderr}")
}

fn tail_text(text: &str, max_lines: usize) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn render_prometheus_config(scrape_interval_secs: u64, targets: &[MetricsTarget]) -> String {
    let mut config = format!(
        "global:\n  scrape_interval: {scrape_interval_secs}s\nscrape_configs:\n  - job_name: prometheus\n    static_configs:\n      - targets: ['127.0.0.1:{PROMETHEUS_PORT}']\n"
    );

    for target in targets {
        config.push_str(&format!(
            "  - job_name: '{}'\n    static_configs:\n      - targets: ['{}']\n",
            quote_yaml_scalar(&target.job),
            quote_yaml_scalar(&target.target)
        ));
    }

    config
}

fn quote_yaml_scalar(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "''")
}

fn render_grafana_datasource(prometheus_url: &str) -> String {
    format!(
        r#"apiVersion: 1
datasources:
  - name: "Devnet Prometheus"
    uid: "{GRAFANA_PROMETHEUS_UID}"
    type: "prometheus"
    access: "proxy"
    url: "{prometheus_url}"
    isDefault: true
    editable: true
"#
    )
}

fn render_grafana_dashboard_provider() -> String {
    format!(
        r#"apiVersion: 1
providers:
  - name: "World Chain Devnet"
    orgId: 1
    folder: "World Chain Devnet"
    folderUid: "world-chain-devnet"
    type: "file"
    disableDeletion: false
    allowUiUpdates: true
    options:
      path: "{GRAFANA_DASHBOARD_DIR}"
      foldersFromFilesStructure: false
"#
    )
}

fn provision_grafana_dashboard(dashboard: &str) -> Result<Vec<u8>> {
    let mut dashboard: Value = serde_json::from_str(dashboard)?;

    if let Some(object) = dashboard.as_object_mut() {
        object.remove("__inputs");
        object.insert("id".to_string(), Value::Null);
    }
    replace_prometheus_datasource_uid(&mut dashboard);

    Ok(serde_json::to_vec_pretty(&dashboard)?)
}

fn replace_prometheus_datasource_uid(value: &mut Value) {
    match value {
        Value::String(value) if value == "${DS_PROMETHEUS}" => {
            *value = GRAFANA_PROMETHEUS_UID.to_string();
        }
        Value::Array(values) => {
            for value in values {
                replace_prometheus_datasource_uid(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                replace_prometheus_datasource_uid(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_config_includes_self_scrape_and_targets() {
        let config = render_prometheus_config(
            7,
            &[MetricsTarget::new(
                "world-chain",
                "host.docker.internal:9001",
            )],
        );

        assert!(config.contains("scrape_interval: 7s"));
        assert!(config.contains("job_name: prometheus"));
        assert!(config.contains("127.0.0.1:9090"));
        assert!(config.contains("job_name: 'world-chain'"));
        assert!(config.contains("host.docker.internal:9001"));
    }

    #[test]
    fn grafana_datasource_uses_stable_prometheus_uid() {
        let config = render_grafana_datasource("http://host.docker.internal:9090");

        assert!(config.contains("uid: \"devnet-prometheus\""));
        assert!(config.contains("url: \"http://host.docker.internal:9090\""));
        assert!(config.contains("isDefault: true"));
    }

    #[test]
    fn grafana_provider_uses_quoted_dashboard_path() {
        let config = render_grafana_dashboard_provider();

        assert!(config.contains("folder: \"World Chain Devnet\""));
        assert!(config.contains(&format!("path: \"{GRAFANA_DASHBOARD_DIR}\"")));
        assert!(config.contains("allowUiUpdates: true"));
    }

    #[test]
    fn grafana_dashboards_are_provisioned_with_devnet_prometheus() {
        let dashboard = provision_grafana_dashboard(FLASHBLOCKS_P2P_DASHBOARD).unwrap();
        let dashboard = String::from_utf8(dashboard).unwrap();

        assert!(!dashboard.contains("__inputs"));
        assert!(!dashboard.contains("${DS_PROMETHEUS}"));
        assert!(dashboard.contains(GRAFANA_PROMETHEUS_UID));
    }
}
