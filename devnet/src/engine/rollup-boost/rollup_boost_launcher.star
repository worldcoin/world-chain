shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)
el_context = import_module(
    "github.com/ethpandaops/ethereum-package/src/el/el_context.star"
)
el_admin_node_info = import_module(
    "github.com/ethpandaops/ethereum-package/src/el/el_admin_node_info.star"
)
constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/constants.star"
)

RPC_PORT_NUM = 8541
WS_PORT_NUM = 8546
DISCOVERY_PORT_NUM = 30303
RPC_PORT_ID = "rpc"


def get_used_ports(discovery_port=DISCOVERY_PORT_NUM):
    used_ports = {
        RPC_PORT_ID: shared_utils.new_port_spec(
            RPC_PORT_NUM,
            shared_utils.TCP_PROTOCOL,
            shared_utils.HTTP_APPLICATION_PROTOCOL,
        ),
    }
    return used_ports


def launch(
    plan,
    launcher,
    service_name,
    image,
    existing_el_clients,
    sequencer_context,
    builder_context,
):
    network_name = shared_utils.get_network_name(launcher.network)

    config = get_config(
        plan,
        launcher.el_cl_genesis_data,
        launcher.jwt_file,
        launcher.network,
        launcher.network_id,
        image,
        service_name,
        existing_el_clients,
        sequencer_context,
        builder_context,
    )

    service = plan.add_service(service_name, config)

    http_url = "http://{0}:{1}".format(service.ip_address, RPC_PORT_NUM)

    return el_context.new_el_context(
        client_name="rollup-boost",
        enode=None,
        ip_addr=service.ip_address,
        rpc_port_num=RPC_PORT_NUM,
        ws_port_num=WS_PORT_NUM,
        engine_rpc_port_num=RPC_PORT_NUM,
        rpc_http_url=http_url,
        service_name=service_name,
        el_metrics_info=None,
    )


def get_config(
    plan,
    el_cl_genesis_data,
    jwt_file,
    network,
    network_id,
    image,
    service_name,
    existing_el_clients,
    sequencer_context,
    builder_context,
):
    used_ports = get_used_ports(DISCOVERY_PORT_NUM)

    public_ports = {}
    cmd = [
        "--builder.http.addr={0}".format(builder_context.ip_addr),
        "--builder.http.port={0}".format(builder_context.rpc_port_num),
        "--builder.auth.addr={0}".format(builder_context.ip_addr),
        "--builder.auth.port={0}".format(builder_context.engine_rpc_port_num),
        "--builder.authrpc.jwtsecret.path={0}".format(constants.JWT_MOUNT_PATH_ON_CONTAINER),
        "--builder.timeout=1000",
        "--l2.http.addr={0}".format(sequencer_context.ip_addr),
        "--l2.http.port={0}".format(sequencer_context.rpc_port_num),
        "--l2.auth.addr={0}".format(sequencer_context.ip_addr),
        "--l2.auth.port={0}".format(sequencer_context.engine_rpc_port_num),
        "--l2.timeout=1000",
        "--l2.authrpc.jwtsecret.path={0}".format(constants.JWT_MOUNT_PATH_ON_CONTAINER),
        "--rpc-port={0}".format(RPC_PORT_NUM),
        "--boost-sync",
        "--log-level=debug",
    ]

    files = {
        constants.JWT_MOUNTPOINT_ON_CLIENTS: jwt_file,
    }

    return ServiceConfig(
        image=image,
        ports=used_ports,
        public_ports=public_ports,
        cmd=cmd,
        files=files,
        private_ip_address_placeholder=constants.PRIVATE_IP_ADDRESS_PLACEHOLDER,
    )


def new_rollup_boost_launcher(
    el_cl_genesis_data,
    jwt_file,
    network,
    network_id,
):
    return struct(
        el_cl_genesis_data=el_cl_genesis_data,
        jwt_file=jwt_file,
        network=network,
        network_id=network_id,
    )
