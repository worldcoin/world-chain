prometheus = import_module("github.com/kurtosis-tech/prometheus-package/main.star")


def launch_prometheus(
    plan,
    observability_helper,
    global_node_selectors,
):
    prometheus_params = observability_helper.params.prometheus_params

    prometheus_url = prometheus.run(
        plan,
        observability_helper.metrics_jobs,
        "prometheus",
        min_cpu=prometheus_params.min_cpu,
        max_cpu=prometheus_params.max_cpu,
        min_memory=prometheus_params.min_mem,
        max_memory=prometheus_params.max_mem,
        node_selectors=global_node_selectors,
        storage_tsdb_retention_time=prometheus_params.storage_tsdb_retention_time,
        storage_tsdb_retention_size=prometheus_params.storage_tsdb_retention_size,
        image=prometheus_params.image,
    )

    return prometheus_url
