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
# The Docker container runs as the "op-proposer" user so we can't write to root
PROPOSER_DATA_DIRPATH_ON_SERVICE_CONTAINER = "/data/op-proposer/op-proposer-data"

# Port IDs
PROPOSER_HTTP_PORT_ID = "http"

# Port nums
PROPOSER_HTTP_PORT_NUM = 8560


def get_used_ports():
    used_ports = {
        PROPOSER_HTTP_PORT_ID: ethereum_package_shared_utils.new_port_spec(
            PROPOSER_HTTP_PORT_NUM,
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
    cl_context,
    l1_config_env_vars,
    gs_proposer_private_key,
    game_factory_address,
    proposer_params,
    observability_helper,
):
    proposer_service_name = "{0}".format(service_name)

    config = get_proposer_config(
        plan,
        image,
        service_name,
        cl_context,
        l1_config_env_vars,
        gs_proposer_private_key,
        game_factory_address,
        proposer_params,
        observability_helper,
    )

    proposer_service = plan.add_service(service_name, config)

    proposer_http_port = proposer_service.ports[PROPOSER_HTTP_PORT_ID]
    proposer_http_url = "http://{0}:{1}".format(
        proposer_service.ip_address, proposer_http_port.number
    )

    observability.register_op_service_metrics_job(
        observability_helper, proposer_service
    )

    return "op_proposer"


def get_proposer_config(
    plan,
    image,
    service_name,
    cl_context,
    l1_config_env_vars,
    gs_proposer_private_key,
    game_factory_address,
    proposer_params,
    observability_helper,
):
    ports = dict(get_used_ports())

    cmd = [
        "op-proposer",
        "--poll-interval=12s",
        "--rpc.port=" + str(PROPOSER_HTTP_PORT_NUM),
        "--rollup-rpc=" + cl_context.beacon_http_url,
        "--game-factory-address=" + str(game_factory_address),
        "--private-key=" + gs_proposer_private_key,
        "--l1-eth-rpc=" + l1_config_env_vars["L1_RPC_URL"],
        "--allow-non-finalized=true",
        "--game-type={0}".format(proposer_params.game_type),
        "--proposal-interval=" + proposer_params.proposal_interval,
        "--wait-node-sync=true",
    ]

    # apply customizations

    if observability_helper.enabled:
        observability.configure_op_service_metrics(cmd, ports)

    cmd += proposer_params.extra_params

    return ServiceConfig(
        image=image,
        ports=ports,
        cmd=cmd,
        private_ip_address_placeholder=ethereum_package_constants.PRIVATE_IP_ADDRESS_PLACEHOLDER,
    )
