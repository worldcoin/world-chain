shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

ethereum_constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/constants.star"
)

RUNDLER_MEMPOOL_CONFIG_MOUNT = "/mempool_config"
RUNDLER_MEMPOOL_CONFIG_MOUNT_PATH = (
    RUNDLER_MEMPOOL_CONFIG_MOUNT + "/mempool_config.json"
)
RUNDLER_BUILDER_CONFIG_MOUNT = "/builder_config"
RUNDLER_BUILDER_CONFIG_MOUNT_PATH = (
    RUNDLER_BUILDER_CONFIG_MOUNT + "/builder_config.json"
)
RUNDLER_CHAIN_SPEC_MOUNT = "/chain_spec"
RUNDLER_CHAIN_SPEC_MOUNT_PATH = RUNDLER_CHAIN_SPEC_MOUNT + "/chain_spec.json"


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
    rpc_http_url,
    builder_config_file,
    mempool_config_file,
    chain_spec_file,
):
    rundler_service_name = "{0}".format(service_name)

    config = get_rundler_config(
        plan,
        image,
        service_name,
        rpc_http_url,
        builder_config_file,
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
    rpc_http_url,
    builder_config_file,
    mempool_config_file,
    chain_spec_file,
):
    cmd = [
        "node",
        "--chain_spec={0}".format(RUNDLER_CHAIN_SPEC_MOUNT_PATH),
        "--signer.private_keys=0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a,0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba",
        "--node_http={0}".format(rpc_http_url),
        "--rpc.port={0}".format(RUNDLER_HTTP_PORT_ID),
        "--unsafe",
        "--da_gas_tracking_enabled",
        "--builders_config_path={0}".format(RUNDLER_BUILDER_CONFIG_MOUNT_PATH),
        "--mempool_config_path={0}".format(RUNDLER_MEMPOOL_CONFIG_MOUNT_PATH),
        "--min_stake_value={0}".format("1"),
        "--min_unstake_delay={0}".format("0"),
        "--disable_entry_point_v0_6",
        "--enabled_aggregators=PBH",
        "--aggregator_options=PBH_ADDRESS=0xcf7ed3acca5a467e9e704c703e8d87f634fb0fc9",
        "--pool.same_sender_mempool_count=255",
    ]

    files = {
        RUNDLER_MEMPOOL_CONFIG_MOUNT: mempool_config_file,
        RUNDLER_BUILDER_CONFIG_MOUNT: builder_config_file,
        RUNDLER_CHAIN_SPEC_MOUNT: chain_spec_file,
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
