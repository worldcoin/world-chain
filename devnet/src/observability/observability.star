ethereum_package_shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

ethereum_package_node_metrics = import_module(
    "github.com/ethpandaops/ethereum-package/src/node_metrics_info.star"
)

DEFAULT_SCRAPE_INTERVAL = "15s"

METRICS_PORT_ID = "metrics"
METRICS_PORT_NUM = 9001
METRICS_PATH = "/debug/metrics/prometheus"

METRICS_INFO_NAME_KEY = "name"
METRICS_INFO_URL_KEY = "url"
METRICS_INFO_PATH_KEY = "path"
METRICS_INFO_ADDITIONAL_CONFIG_KEY = "config"


def make_metrics_url(service, metrics_port_num=METRICS_PORT_NUM):
    return "{0}:{1}".format(service.ip_address, metrics_port_num)


def new_metrics_info(helper, service, metrics_path=METRICS_PATH):
    if not helper.enabled:
        return None

    metrics_url = make_metrics_url(service)
    metrics_info = ethereum_package_node_metrics.new_node_metrics_info(
        service.name, metrics_path, metrics_url
    )

    return metrics_info


def expose_metrics_port(ports, port_id=METRICS_PORT_ID, port_num=METRICS_PORT_NUM):
    ports[port_id] = ethereum_package_shared_utils.new_port_spec(
        port_num, ethereum_package_shared_utils.TCP_PROTOCOL
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


def register_op_service_metrics_job(helper, service):
    register_service_metrics_job(
        helper,
        service_name=service.name,
        endpoint=make_metrics_url(service),
    )


def register_service_metrics_job(
    helper,
    service_name,
    endpoint,
    metrics_path="",
    additional_labels={},
    scrape_interval=DEFAULT_SCRAPE_INTERVAL,
):
    labels = {
        "service": service_name,
        "namespace": "kurtosis",
        "stack_optimism_io_network": "kurtosis",
    }
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
    helper, client_name, client_type, node_metrics_info, additional_labels={}
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
        endpoint=node_metrics_info[METRICS_INFO_URL_KEY],
        metrics_path=node_metrics_info[METRICS_INFO_PATH_KEY],
        additional_labels=labels,
        scrape_interval=scrape_interval,
    )
