ethereum_package_shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

ethereum_package_el_context = import_module(
    "github.com/ethpandaops/ethereum-package/src/el/el_context.star"
)

ethereum_package_el_admin_node_info = import_module(
    "github.com/ethpandaops/ethereum-package/src/el/el_admin_node_info.star"
)

ethereum_package_node_metrics = import_module(
    "github.com/ethpandaops/ethereum-package/src/node_metrics_info.star"
)

ethereum_package_input_parser = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/input_parser.star"
)

ethereum_package_constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/constants.star"
)

constants = import_module("../../package_io/constants.star")
observability = import_module("../../observability/observability.star")
util = import_module("../../util.star")

RPC_PORT_NUM = 8545
WS_PORT_NUM = 8546
DISCOVERY_PORT_NUM = 30303
ENGINE_RPC_PORT_NUM = 8551

# The min/max CPU/memory that the execution node can use
EXECUTION_MIN_CPU = 300
EXECUTION_MIN_MEMORY = 512

# Port IDs
RPC_PORT_ID = "rpc"
WS_PORT_ID = "ws"
TCP_DISCOVERY_PORT_ID = "tcp-discovery"
UDP_DISCOVERY_PORT_ID = "udp-discovery"
ENGINE_RPC_PORT_ID = "engine-rpc"
ENGINE_WS_PORT_ID = "engineWs"

# TODO(old) Scale this dynamically based on CPUs available and Geth nodes mining
NUM_MINING_THREADS = 1

# The dirpath of the execution data directory on the client container
EXECUTION_DATA_DIRPATH_ON_CLIENT_CONTAINER = "/data/besu/execution-data"


def get_used_ports(discovery_port=DISCOVERY_PORT_NUM):
    used_ports = {
        RPC_PORT_ID: ethereum_package_shared_utils.new_port_spec(
            RPC_PORT_NUM,
            ethereum_package_shared_utils.TCP_PROTOCOL,
            ethereum_package_shared_utils.HTTP_APPLICATION_PROTOCOL,
        ),
        WS_PORT_ID: ethereum_package_shared_utils.new_port_spec(
            WS_PORT_NUM, ethereum_package_shared_utils.TCP_PROTOCOL
        ),
        TCP_DISCOVERY_PORT_ID: ethereum_package_shared_utils.new_port_spec(
            discovery_port, ethereum_package_shared_utils.TCP_PROTOCOL
        ),
        UDP_DISCOVERY_PORT_ID: ethereum_package_shared_utils.new_port_spec(
            discovery_port, ethereum_package_shared_utils.UDP_PROTOCOL
        ),
        ENGINE_RPC_PORT_ID: ethereum_package_shared_utils.new_port_spec(
            ENGINE_RPC_PORT_NUM,
            ethereum_package_shared_utils.TCP_PROTOCOL,
        ),
    }
    return used_ports


ENTRYPOINT_ARGS = ["sh", "-c"]

VERBOSITY_LEVELS = {
    ethereum_package_constants.GLOBAL_LOG_LEVEL.error: "1",
    ethereum_package_constants.GLOBAL_LOG_LEVEL.warn: "2",
    ethereum_package_constants.GLOBAL_LOG_LEVEL.info: "3",
    ethereum_package_constants.GLOBAL_LOG_LEVEL.debug: "4",
    ethereum_package_constants.GLOBAL_LOG_LEVEL.trace: "5",
}

BUILDER_IMAGE_STR = "builder"
SUAVE_ENABLED_GETH_IMAGE_STR = "suave"


def launch(
    plan,
    launcher,
    service_name,
    participant,
    global_log_level,
    persistent,
    tolerations,
    node_selectors,
    existing_el_clients,
    sequencer_enabled,
    sequencer_context,
    observability_helper,
    interop_params,
):
    log_level = ethereum_package_input_parser.get_client_log_level_or_default(
        participant.el_log_level, global_log_level, VERBOSITY_LEVELS
    )

    cl_client_name = service_name.split("-")[4]

    config = get_config(
        plan,
        launcher,
        service_name,
        participant,
        log_level,
        persistent,
        tolerations,
        node_selectors,
        existing_el_clients,
        cl_client_name,
        sequencer_enabled,
        sequencer_context,
        observability_helper,
    )

    service = plan.add_service(service_name, config)

    enode = ethereum_package_el_admin_node_info.get_enode_for_node(
        plan, service_name, RPC_PORT_ID
    )

    http_url = "http://{0}:{1}".format(service.ip_address, RPC_PORT_NUM)

    metrics_info = observability.new_metrics_info(observability_helper, service)

    return ethereum_package_el_context.new_el_context(
        client_name="op-besu",
        enode=enode,
        ip_addr=service.ip_address,
        rpc_port_num=RPC_PORT_NUM,
        ws_port_num=WS_PORT_NUM,
        engine_rpc_port_num=ENGINE_RPC_PORT_NUM,
        rpc_http_url=http_url,
        service_name=service_name,
        el_metrics_info=[metrics_info],
    )


def get_config(
    plan,
    launcher,
    service_name,
    participant,
    log_level,
    persistent,
    tolerations,
    node_selectors,
    existing_el_clients,
    cl_client_name,
    sequencer_enabled,
    sequencer_context,
    observability_helper,
):
    discovery_port = DISCOVERY_PORT_NUM
    ports = dict(get_used_ports(discovery_port))

    cmd = [
        "besu",
        "--genesis-file="
        + "{0}/genesis-{1}.json".format(
            ethereum_package_constants.GENESIS_CONFIG_MOUNT_PATH_ON_CONTAINER,
            launcher.network_id,
        ),
        "--network-id={0}".format(launcher.network_id),
        # "--logging=" + log_level,
        "--data-path=" + EXECUTION_DATA_DIRPATH_ON_CLIENT_CONTAINER,
        "--host-allowlist=*",
        "--rpc-http-enabled=true",
        "--rpc-http-host=0.0.0.0",
        "--rpc-http-port={0}".format(RPC_PORT_NUM),
        "--rpc-http-api=ADMIN,CLIQUE,ETH,NET,DEBUG,TXPOOL,ENGINE,TRACE,WEB3,MINER",
        "--rpc-http-cors-origins=*",
        "--rpc-http-max-active-connections=300",
        "--rpc-ws-enabled=true",
        "--rpc-ws-host=0.0.0.0",
        "--rpc-ws-port={0}".format(WS_PORT_NUM),
        "--rpc-ws-api=ADMIN,CLIQUE,ETH,NET,DEBUG,TXPOOL,ENGINE,TRACE,WEB3,MINER",
        "--p2p-enabled=true",
        "--p2p-host=" + ethereum_package_constants.PRIVATE_IP_ADDRESS_PLACEHOLDER,
        "--p2p-port={0}".format(discovery_port),
        "--engine-rpc-enabled=true",
        "--engine-jwt-secret=" + ethereum_package_constants.JWT_MOUNT_PATH_ON_CONTAINER,
        "--engine-host-allowlist=*",
        "--engine-rpc-port={0}".format(ENGINE_RPC_PORT_NUM),
        "--sync-mode=FULL",
        "--bonsai-limit-trie-logs-enabled=false",
        "--version-compatibility-protection=false",
    ]

    # configure files

    files = {
        ethereum_package_constants.GENESIS_DATA_MOUNTPOINT_ON_CLIENTS: launcher.deployment_output,
        ethereum_package_constants.JWT_MOUNTPOINT_ON_CLIENTS: launcher.jwt_file,
    }
    if persistent:
        files[EXECUTION_DATA_DIRPATH_ON_CLIENT_CONTAINER] = Directory(
            persistent_key="data-{0}".format(service_name),
            size=int(participant.el_volume_size)
            if int(participant.el_volume_size) > 0
            else constants.VOLUME_SIZE[launcher.network][
                constants.EL_TYPE.op_besu + "_volume_size"
            ],
        )

    # configure environment variables

    env_vars = dict(participant.el_extra_env_vars)

    # apply customizations

    if observability_helper.enabled:
        cmd += [
            "--metrics-enabled=true",
            "--metrics-host=0.0.0.0",
            "--metrics-port={0}".format(observability.METRICS_PORT_NUM),
        ]

        observability.expose_metrics_port(ports)

    # if not sequencer_enabled:
    #     cmd.append(
    #         "--rollup.sequencerhttp={0}".format(sequencer_context.rpc_http_url)
    #     )

    if len(existing_el_clients) > 0:
        cmd.append(
            "--bootnodes="
            + ",".join(
                [
                    ctx.enode
                    for ctx in existing_el_clients[
                        : ethereum_package_constants.MAX_ENODE_ENTRIES
                    ]
                ]
            )
        )

    cmd += participant.el_extra_params
    cmd_str = " ".join(cmd)

    config_args = {
        "image": participant.el_image,
        "ports": ports,
        "cmd": [cmd_str],
        "files": files,
        "entrypoint": ENTRYPOINT_ARGS,
        "private_ip_address_placeholder": ethereum_package_constants.PRIVATE_IP_ADDRESS_PLACEHOLDER,
        "env_vars": env_vars,
        "labels": ethereum_package_shared_utils.label_maker(
            client=constants.EL_TYPE.op_besu,
            client_type=constants.CLIENT_TYPES.el,
            image=util.label_from_image(participant.el_image),
            connected_client=cl_client_name,
            extra_labels=participant.el_extra_labels,
        ),
        "tolerations": tolerations,
        "node_selectors": node_selectors,
        "user": User(uid=0, gid=0),
    }

    # configure resources

    if participant.el_min_cpu > 0:
        config_args["min_cpu"] = participant.el_min_cpu
    if participant.el_max_cpu > 0:
        config_args["max_cpu"] = participant.el_max_cpu
    if participant.el_min_mem > 0:
        config_args["min_memory"] = participant.el_min_mem
    if participant.el_max_mem > 0:
        config_args["max_memory"] = participant.el_max_mem

    return ServiceConfig(**config_args)


def new_op_besu_launcher(
    deployment_output,
    jwt_file,
    network,
    network_id,
):
    return struct(
        deployment_output=deployment_output,
        jwt_file=jwt_file,
        network=network,
        network_id=network_id,
    )
