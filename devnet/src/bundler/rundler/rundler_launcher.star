shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

ethereum_constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/constants.star"
)

rundler_constants = import_module(
    "../../package_io/constants.star"
)

#
#  ---------------------------------- Rundler client -------------------------------------

RUNDLER_HTTP_PORT_ID = 8453
DISCOVERY_PORT_NUM = 30303
RPC_PORT_ID = "rpc"

def get_used_ports(discovery_port=DISCOVERY_PORT_NUM):
    used_ports = {
        RPC_PORT_ID: shared_utils.new_port_spec(
            RUNDLER_HTTP_PORT_ID,
            shared_utils.TCP_PROTOCOL,
            shared_utils.HTTP_APPLICATION_PROTOCOL,
        ),
    }
    return used_ports


ENTRYPOINT_ARGS = ["sh", "-c"]


def launch(
    plan,
    service_name,
    image,
    el_context,
    entrypoint_config_file,
    mempool_config_file,
    chain_spec_file,
):
    rundler_service_name = "{0}".format(service_name)

    config = get_rundler_config(
        plan,
        image,
        service_name,
        el_context,
        entrypoint_config_file,
        mempool_config_file,
        chain_spec_file,
    )

    rundler_service = plan.add_service(service_name, config)

    rundler_http_url = "http://{0}:{1}".format(
        rundler_service.ip_address, RUNDLER_HTTP_PORT_ID
    )

    return "rundler"


def get_rundler_config(
    plan,
    image,
    service_name,
    el_context,
    entrypoint_config_file,
    mempool_config_file,
    chain_spec_file,
):
    cmd = [
        "node",
        "--chain_spec={0}".format(rundler_constants.CHAIN_SPEC_MOUNT_PATH),
        "--builder.private_keys=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80,0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        "--node_http={0}".format(el_context.rpc_http_url), # rollup-boost RPC server
        "--rpc.port={0}".format(RUNDLER_HTTP_PORT_ID),
        "--builder.dropped_status_unsupported",
        "--unsafe",
        "--da_gas_tracking_enabled",
        "--entry_point_builders_path={0}".format(rundler_constants.ENTRYPOINT_CONFIG_MOUNT_PATH),
        "--mempool_config_path={0}".format(rundler_constants.MEMPOOL_CONFIG_MOUNT_PATH),
        "--min_stake_value={0}".format("1"),
        "--min_unstake_delay={0}".format("0"),
        "--disable_entry_point_v0_6",
        "--enabled_aggregators=PBH",
        "--pool.same_sender_mempool_count=255"
    ]

    files = {
        rundler_constants.MEMPOOL_CONFIG_MOUNT: mempool_config_file,
        rundler_constants.ENTRYPOINT_CONFIG_MOUNT: entrypoint_config_file,
        rundler_constants.CHAIN_SPEC_MOUNT: chain_spec_file,
    }

    env_vars = {
        "RUST_LOG": "INFO",
    }

    ports = get_used_ports()
    return ServiceConfig(
        image=image,
        ports=ports,
        cmd=cmd,
        files=files,
        env_vars=env_vars,
        private_ip_address_placeholder=ethereum_constants.PRIVATE_IP_ADDRESS_PLACEHOLDER,
    )
