ethereum_package_shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

ethereum_package_node_metrics = import_module(
    "github.com/ethpandaops/ethereum-package/src/node_metrics_info.star"
)

util = import_module("../util.star")

prometheus = import_module("./prometheus/prometheus_launcher.star")
loki = import_module("./loki/loki_launcher.star")
promtail = import_module("./promtail/promtail_launcher.star")
grafana = import_module("./grafana/grafana_launcher.star")


DEFAULT_SCRAPE_INTERVAL = "15s"

METRICS_PORT_ID = "metrics"
METRICS_PORT_NUM = 9001
METRICS_PATH = "/debug/metrics/prometheus"

METRICS_INFO_NAME_KEY = "name"
METRICS_INFO_URL_KEY = "url"
METRICS_INFO_PATH_KEY = "path"
METRICS_INFO_ADDITIONAL_CONFIG_KEY = "config"


def new_metrics_info(helper, service, metrics_path=METRICS_PATH):
    if not helper.enabled:
        return None

    metrics_url = util.make_service_url_authority(service, METRICS_PORT_ID)
    metrics_info = ethereum_package_node_metrics.new_node_metrics_info(
        service.name, metrics_path, metrics_url
    )

    return metrics_info


def expose_metrics_port(ports, port_id=METRICS_PORT_ID, port_num=METRICS_PORT_NUM):
    ports[port_id] = ethereum_package_shared_utils.new_port_spec(
        port_num,
        ethereum_package_shared_utils.TCP_PROTOCOL,
        ethereum_package_shared_utils.HTTP_APPLICATION_PROTOCOL,
    )


# configures the CLI flags and ports for a service using the standard op-service setup
def configure_op_service_metrics(cmd, ports):
    cmd += [
        "--metrics.enabled",
        "--metrics.addr=0.0.0.0",
        "--metrics.port={0}".format(METRICS_PORT_NUM),
    ]

    expose_metrics_port(ports)


def make_helper(observability_params):
    return struct(
        params=observability_params,
        enabled=observability_params.enabled,
        metrics_jobs=[],
    )


def add_metrics_job(helper, job):
    helper.metrics_jobs.append(job)


def new_metrics_job(
    job_name,
    endpoint,
    metrics_path,
    labels,
    scrape_interval=DEFAULT_SCRAPE_INTERVAL,
):
    return {
        "Name": job_name,
        "Endpoint": endpoint,
        "MetricsPath": metrics_path,
        "Labels": labels,
        "ScrapeInterval": scrape_interval,
    }


def register_op_service_metrics_job(helper, service, network_name=None):
    register_service_metrics_job(
        helper,
        service_name=service.name,
        network_name=network_name,
        endpoint=util.make_service_url_authority(service, METRICS_PORT_ID),
    )


def register_service_metrics_job(
    helper,
    service_name,
    endpoint,
    network_name=None,
    metrics_path="",
    additional_labels={},
    scrape_interval=DEFAULT_SCRAPE_INTERVAL,
):
    labels = {
        "service": service_name,
        "namespace": service_name,
    }
    if network_name != None:
        labels["stack_optimism_io_network"] = network_name

    labels.update(additional_labels)

    add_metrics_job(
        helper,
        new_metrics_job(
            job_name=service_name,
            endpoint=endpoint,
            metrics_path=metrics_path,
            labels=labels,
            scrape_interval=scrape_interval,
        ),
    )


def register_node_metrics_job(
    helper,
    client_name,
    client_type,
    network_name,
    node_metrics_info,
    additional_labels={},
):
    labels = {
        "client_type": client_type,
        "client_name": client_name,
    }
    labels.update(additional_labels)

    scrape_interval = DEFAULT_SCRAPE_INTERVAL

    additional_config = node_metrics_info[METRICS_INFO_ADDITIONAL_CONFIG_KEY]

    if additional_config != None:
        if additional_config.labels != None:
            labels.update(additional_config.labels)

        if (
            additional_config.scrape_interval != None
            and additional_config.scrape_interval != ""
        ):
            scrape_interval = additional_config.scrape_interval

    register_service_metrics_job(
        helper,
        service_name=node_metrics_info[METRICS_INFO_NAME_KEY],
        network_name=network_name,
        endpoint=node_metrics_info[METRICS_INFO_URL_KEY],
        metrics_path=node_metrics_info[METRICS_INFO_PATH_KEY],
        additional_labels=labels,
        scrape_interval=scrape_interval,
    )


def launch(plan, observability_helper, global_node_selectors, observability_params):
    if not observability_helper.enabled or len(observability_helper.metrics_jobs) == 0:
        return

    plan.print("Launching prometheus...")
    prometheus_private_url = prometheus.launch_prometheus(
        plan,
        observability_helper,
        global_node_selectors,
    )

    loki_url = None
    if observability_params.enable_k8s_features:
        plan.print("Launching loki...")
        loki_url = loki.launch_loki(
            plan,
            global_node_selectors,
            observability_params.loki_params,
        )

        plan.print("Launching promtail...")
        promtail.launch_promtail(
            plan,
            global_node_selectors,
            loki_url,
            observability_params.promtail_params,
        )

    plan.print("Launching grafana...")
    grafana.launch_grafana(
        plan,
        prometheus_private_url,
        loki_url,
        global_node_selectors,
        observability_params.grafana_params,
    )
