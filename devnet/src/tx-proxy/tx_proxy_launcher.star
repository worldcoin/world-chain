shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

ethereum_constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/constants.star"
)

#
#  ---------------------------------- Tx Proxy client -------------------------------------

TX_PROXY_HTTP_PORT = 8080
TX_PROXY_HTTP_ADDRESS = "0.0.0.0"
DISCOVERY_PORT_NUM = 30303
RPC_PORT_ID = "rpc"


def get_used_ports(discovery_port=DISCOVERY_PORT_NUM):
    used_ports = {
        RPC_PORT_ID: shared_utils.new_port_spec(
            TX_PROXY_HTTP_PORT,
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
    builder_rpc_0,
    builder_rpc_1,
    builder_rpc_2,
    l2_rpc_0,
    l2_rpc_1,
    l2_rpc_2,
    jwt_file,
):
    tx_proxy_service_name = "{0}".format(service_name)

    config = get_tx_proxy_config(
        plan,
        service_name,
        image,
        builder_rpc_0,
        builder_rpc_1,
        builder_rpc_2,
        l2_rpc_0,
        l2_rpc_1,
        l2_rpc_2,
        jwt_file,
    )

    tx_proxy_service = plan.add_service(service_name, config)

    tx_proxy_http_url = "http://{0}:{1}".format(
        tx_proxy_service.ip_address, TX_PROXY_HTTP_PORT
    )

    return tx_proxy_http_url


def get_tx_proxy_config(
    plan,
    service_name,
    image,
    builder_rpc_0,
    builder_rpc_1,
    builder_rpc_2,
    l2_rpc_0,
    l2_rpc_1,
    l2_rpc_2,
    jwt_file,
):
    cmd = [
        "--builder-url-0={0}".format(builder_rpc_0),
        "--builder-url-1={0}".format(builder_rpc_1),
        "--builder-url-2={0}".format(builder_rpc_2),
        "--builder-jwt-path={0}".format(ethereum_constants.JWT_MOUNT_PATH_ON_CONTAINER),
        "--l2-url-0={0}".format(l2_rpc_0),
        "--l2-url-1={0}".format(l2_rpc_1),
        "--l2-url-2={0}".format(l2_rpc_2),
        "--l2-jwt-path={0}".format(ethereum_constants.JWT_MOUNT_PATH_ON_CONTAINER),
        "--http-port={0}".format(TX_PROXY_HTTP_PORT),
        "--http-addr={0}".format(TX_PROXY_HTTP_ADDRESS),
        "--tracing",
        "--log-level={0}".format("debug"),
    ]

    env_vars = {
        "RUST_LOG": "info, tx-proxy=debug",
    }

    ports = get_used_ports()

    files = {
        ethereum_constants.JWT_MOUNTPOINT_ON_CLIENTS: jwt_file,
    }

    return ServiceConfig(
        image=image,
        ports=ports,
        cmd=cmd,
        files=files,
        env_vars=env_vars,
        private_ip_address_placeholder=ethereum_constants.PRIVATE_IP_ADDRESS_PLACEHOLDER,
    )
