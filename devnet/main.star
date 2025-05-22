optimism_package = import_module(
    "github.com/ethpandaops/optimism-package/main.star@5b46ce33ca331fd595d5b9dc9eea004ab3522f1b"
)

world_chain_builder = import_module("./src/el/world_chain_builder_launcher.star")

rundler = import_module("./src/bundler/rundler/rundler_launcher.star")
static_files = import_module("./src/static_files/static_files.star")

tx_proxy = import_module("./src/tx-proxy/tx_proxy_launcher.star")


# TODO: HA Deployment with op-conductor
def run(plan, args={}):
    optimism_package.run(
        plan,
        args,
        custom_launchers={
            "el_builder_launcher": {
                "launcher": world_chain_builder.new_op_reth_builder_launcher,
                "launch_method": world_chain_builder.launch,
            },
        },
    )

    rundler_builder_config_file = plan.upload_files(
        src=static_files.RUNDLER_BUILDER_CONFIG_FILE_PATH,
        name="builder_config.json",
    )
    rundler_mempool_config_file = plan.upload_files(
        src=static_files.RUNDLER_MEMPOOL_CONFIG_FILE_PATH,
        name="mempool_config.json",
    )
    rundler_chain_spec = plan.upload_files(
        src=static_files.RUNDLER_CHAIN_SPEC_FILE_PATH,
        name="chain_spec.json",
    )

    jwt_file = plan.upload_files(
        src=static_files.JWT_FILE_PATH,
        name="jwtsecret",
    )

    # Extract HTTP RPC url of the builder
    builder_srv = plan.get_service("op-el-builder-2151908-1-custom-op-node-op-kurtosis")
    builder_rpc_port = builder_srv.ports["rpc"].number
    builder_rpc_url = "http://{0}:{1}".format(builder_srv.ip_address, builder_rpc_port)

    # Extract HTTP RPC url of 2 Reth nodes
    reth_srv_0 = plan.get_service("op-el-2151908-2-op-reth-op-node-op-kurtosis")
    reth_rpc_port_0 = reth_srv_0.ports["rpc"].number
    reth_rpc_url_0 = "http://{0}:{1}".format(reth_srv_0.ip_address, reth_rpc_port_0)

    reth_srv_1 = plan.get_service("op-el-2151908-3-op-reth-op-node-op-kurtosis")
    reth_rpc_port_1 = reth_srv_1.ports["rpc"].number
    reth_rpc_url_1 = "http://{0}:{1}".format(reth_srv_1.ip_address, reth_rpc_port_1)

    l2_srv = plan.get_service("op-el-2151908-1-op-geth-op-node-op-kurtosis")
    l2_rpc_port = l2_srv.ports["rpc"].number
    l2_rpc_url = "http://{0}:{1}".format(l2_srv.ip_address, l2_rpc_port)

    tx_proxy_http_url = tx_proxy.launch(
        plan,
        service_name="tx-proxy",
        image="ghcr.io/worldcoin/tx-proxy:sha-7b4770d",
        builder_rpc_0=builder_rpc_url,
        builder_rpc_1=reth_rpc_url_0,  # need to be separate client to prevent validation errors
        builder_rpc_2=reth_rpc_url_1,
        l2_rpc_0=l2_rpc_url,
        l2_rpc_1=l2_rpc_url,
        l2_rpc_2=l2_rpc_url,
        jwt_file=jwt_file,
    )

    rundler.launch(
        plan,
        service_name="rundler",
        image="alchemyplatform/rundler:v0.8.2",
        rpc_http_url=builder_rpc_url,
        builder_config_file=rundler_builder_config_file,
        mempool_config_file=rundler_mempool_config_file,
        chain_spec_file=rundler_chain_spec,
    )
