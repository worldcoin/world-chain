ethereum_package_shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

ethereum_package_constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/constants.star"
)

observability = import_module("../../observability/observability.star")
prometheus = import_module("../../observability/prometheus/prometheus_launcher.star")

#
#  ---------------------------------- Batcher client -------------------------------------
# The Docker container runs as the "op-batcher" user so we can't write to root
BATCHER_DATA_DIRPATH_ON_SERVICE_CONTAINER = "/data/op-batcher/op-batcher-data"

# Port IDs
BATCHER_HTTP_PORT_ID = "http"

# Port nums
BATCHER_HTTP_PORT_NUM = 8548


def get_used_ports():
    used_ports = {
        BATCHER_HTTP_PORT_ID: ethereum_package_shared_utils.new_port_spec(
            BATCHER_HTTP_PORT_NUM,
            ethereum_package_shared_utils.TCP_PROTOCOL,
            ethereum_package_shared_utils.HTTP_APPLICATION_PROTOCOL,
        ),
    }
    return used_ports


ENTRYPOINT_ARGS = ["sh", "-c"]


def launch(
    plan,
    service_name,
    image,
    el_context,
    cl_context,
    l1_config_env_vars,
    gs_batcher_private_key,
    batcher_params,
    observability_helper,
    da_server_context,
):
    batcher_service_name = "{0}".format(service_name)

    config = get_batcher_config(
        plan,
        image,
        service_name,
        el_context,
        cl_context,
        l1_config_env_vars,
        gs_batcher_private_key,
        batcher_params,
        observability_helper,
        da_server_context,
    )

    batcher_service = plan.add_service(service_name, config)

    batcher_http_port = batcher_service.ports[BATCHER_HTTP_PORT_ID]
    batcher_http_url = "http://{0}:{1}".format(
        batcher_service.ip_address, batcher_http_port.number
    )

    observability.register_op_service_metrics_job(observability_helper, batcher_service)

    return "op_batcher"


def get_batcher_config(
    plan,
    image,
    service_name,
    el_context,
    cl_context,
    l1_config_env_vars,
    gs_batcher_private_key,
    batcher_params,
    observability_helper,
    da_server_context,
):
    ports = dict(get_used_ports())

    cmd = [
        "op-batcher",
        "--l2-eth-rpc=" + el_context.rpc_http_url,
        "--rollup-rpc=" + cl_context.beacon_http_url,
        "--poll-interval=1s",
        "--sub-safety-margin=6",
        "--num-confirmations=1",
        "--safe-abort-nonce-too-low-count=3",
        "--resubmission-timeout=30s",
        "--rpc.addr=0.0.0.0",
        "--rpc.port=" + str(BATCHER_HTTP_PORT_NUM),
        "--rpc.enable-admin",
        "--max-channel-duration=1",
        "--l1-eth-rpc=" + l1_config_env_vars["L1_RPC_URL"],
        "--private-key=" + gs_batcher_private_key,
        # da commitments currently have to be sent as calldata to the batcher inbox
        "--data-availability-type="
        + ("calldata" if da_server_context.enabled else "blobs"),
        "--altda.enabled=" + str(da_server_context.enabled),
        "--altda.da-server=" + da_server_context.http_url,
        # This flag is very badly named, but is needed in order to let the da-server compute the commitment.
        # This leads to sending POST requests to /put instead of /put/<keccak256(data)>
        "--altda.da-service",
    ]

    # apply customizations

    if observability_helper.enabled:
        observability.configure_op_service_metrics(cmd, ports)

    cmd += batcher_params.extra_params

    return ServiceConfig(
        image=image,
        ports=ports,
        cmd=cmd,
        private_ip_address_placeholder=ethereum_package_constants.PRIVATE_IP_ADDRESS_PLACEHOLDER,
    )
