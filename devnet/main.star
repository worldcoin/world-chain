optimism_package = import_module(
    "github.com/ethpandaops/optimism-package/main.star@5ec4fe7972a362ca7408e7fbb47d76805352571b"
)

world_chain_builder = import_module("./src/el/world_chain_builder_launcher.star")

rundler = import_module("./src/bundler/rundler/rundler_launcher.star")
static_files = import_module("./src/static_files/static_files.star")

tx_proxy = import_module("./src/tx-proxy/tx_proxy_launcher.star")
rollup_boost = import_module("./src/rollup-boost/launcher.star")


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
            "sidecar_launcher": {
                "launcher": rollup_boost.new_rollup_boost_launcher,
                "launch_method": rollup_boost.launch,
            },
            "el_launcher": {
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

    # Stop the builder op-node service
    plan.stop_service("op-cl-builder-2151908-1-op-node-custom-op-kurtosis")

    # Extract HTTP RPC url of the builder
    builder_srv = plan.get_service("op-el-builder-2151908-1-custom-op-node-op-kurtosis")
    builder_rpc_port = builder_srv.ports["rpc"].number
    builder_rpc_url = "http://{0}:{1}".format(builder_srv.ip_address, builder_rpc_port)

    l2_srv = plan.get_service("op-el-2151908-1-op-geth-op-node-op-kurtosis")
    l2_rpc_port = l2_srv.ports["rpc"].number
    l2_rpc_url = "http://{0}:{1}".format(l2_srv.ip_address, l2_rpc_port)

    # Add the builders as trusted peers with one another
    builder_0_srv = plan.get_service(
        "op-el-builder-2151908-1-custom-op-node-op-kurtosis"
    )

    builder_1_srv = plan.get_service("op-el-2151908-2-custom-op-node-op-kurtosis")
    builder_1_rpc_port = builder_1_srv.ports["rpc"].number
    builder_1_rpc_url = "http://{0}:{1}".format(
        builder_1_srv.ip_address, builder_1_rpc_port
    )

    builder_2_srv = plan.get_service("op-el-2151908-3-custom-op-node-op-kurtosis")
    builder_2_rpc_port = builder_2_srv.ports["rpc"].number
    builder_2_rpc_url = "http://{0}:{1}".format(
        builder_2_srv.ip_address, builder_2_rpc_port
    )

    extract_enode_recipe = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"admin_nodeInfo","params":[],"id":1}',
        port_id="rpc",
        extract={"enode": ".result.enode"},
    )

    builder_0_enode = plan.request(
        service_name="op-el-builder-2151908-1-custom-op-node-op-kurtosis",
        recipe=extract_enode_recipe,
        description="Extracting enode from builder 0",
    )

    builder_1_enode = plan.request(
        service_name="op-el-2151908-2-custom-op-node-op-kurtosis",
        recipe=extract_enode_recipe,
        description="Extracting enode from builder 1",
    )

    builder_2_enode = plan.request(
        service_name="op-el-2151908-3-custom-op-node-op-kurtosis",
        recipe=extract_enode_recipe,
        description="Extracting enode from builder 2",
    )

    add_trusted_peer_0_recipe = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"admin_addTrustedPeer","params":['
        + '"'
        + "{0}".format(builder_0_enode["extract.enode"])
        + '"'
        + '],"id":1}',
        port_id="rpc",
    )
    
    add_trusted_peer_1_recipe = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"admin_addTrustedPeer","params":['
        + '"'
        + "{0}".format(builder_0_enode["extract.enode"])
        + '"'
        + '],"id":1}',
        port_id="rpc",
    )

    add_trusted_peer_2_recipe = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"admin_addTrustedPeer","params":['
        + '"'
        + "{0}".format(builder_1_enode["extract.enode"])
        + '"'
        + '],"id":1}',
        port_id="rpc",
    )

    add_trusted_peer_3_recipe = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"admin_addTrustedPeer","params":['
        + '"'
        + "{0}".format(builder_2_enode["extract.enode"])
        + '"'
        + '],"id":1}',
        port_id="rpc",
    )


    plan.request(
        service_name="op-el-2151908-2-custom-op-node-op-kurtosis",
        recipe=add_trusted_peer_0_recipe,
        description="Adding trusted peers to the builders",
    )

    plan.request(
        service_name="op-el-2151908-3-custom-op-node-op-kurtosis",
        recipe=add_trusted_peer_1_recipe,
        description="Adding trusted peers to the builders",
    )

    plan.request(
        service_name="op-el-builder-2151908-1-custom-op-node-op-kurtosis",
        recipe=add_trusted_peer_2_recipe,
        description="Adding trusted peers to the builders",
    )

    plan.request(
        service_name="op-el-builder-2151908-1-custom-op-node-op-kurtosis",
        recipe=add_trusted_peer_3_recipe,
        description="Adding trusted peers to the builders",
    )

    # Peer op-node clients together using P2P admin API
    # Get the three op-node services
    op_node_1_srv = plan.get_service("op-cl-2151908-1-op-node-op-geth-op-kurtosis")
    op_node_2_srv = plan.get_service("op-cl-2151908-2-op-node-custom-op-kurtosis")
    op_node_3_srv = plan.get_service("op-cl-2151908-3-op-node-custom-op-kurtosis")

    # Extract the p2p enode/ENR from each op-node
    extract_p2p_info_recipe = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"opp2p_self","params":[],"id":1}',
        port_id="http",
        extract={"peer_id": ".result.addresses[0]"},
    )

    op_node_1_p2p = plan.request(
        service_name="op-cl-2151908-1-op-node-op-geth-op-kurtosis",
        recipe=extract_p2p_info_recipe,
        description="Extracting P2P info from op-node-1",
    )

    op_node_2_p2p = plan.request(
        service_name="op-cl-2151908-2-op-node-custom-op-kurtosis",
        recipe=extract_p2p_info_recipe,
        description="Extracting P2P info from op-node-2",
    )

    op_node_3_p2p = plan.request(
        service_name="op-cl-2151908-3-op-node-custom-op-kurtosis",
        recipe=extract_p2p_info_recipe,
        description="Extracting P2P info from op-node-3",
    )

    # Connect op-node-2 to op-node-1
    connect_peer_recipe_2_to_1 = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"opp2p_connectPeer","params":["' + op_node_1_p2p["extract.peer_id"] + '"],"id":1}',
        port_id="http",
    )

    # Connect op-node-1 to op-node-2 (bidirectional for redundancy)
    connect_peer_recipe_1_to_2 = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"opp2p_connectPeer","params":["' + op_node_2_p2p["extract.peer_id"] + '"],"id":1}',
        port_id="http",
    )

    # Connect op-node-3 to op-node-1
    connect_peer_recipe_3_to_1 = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"opp2p_connectPeer","params":["' + op_node_1_p2p["extract.peer_id"] + '"],"id":1}',
        port_id="http",
    )

    # Connect op-node-1 to op-node-3 (bidirectional for redundancy)
    connect_peer_recipe_1_to_3 = PostHttpRequestRecipe(
        endpoint="/",
        content_type="application/json",
        body='{"jsonrpc":"2.0","method":"opp2p_connectPeer","params":["' + op_node_3_p2p["extract.peer_id"] + '"],"id":1}',
        port_id="http",
    )

    plan.request(
        service_name="op-cl-2151908-2-op-node-custom-op-kurtosis",
        recipe=connect_peer_recipe_2_to_1,
        description="Connecting op-node-2 to op-node-1",
    )

    plan.request(
        service_name="op-cl-2151908-1-op-node-op-geth-op-kurtosis",
        recipe=connect_peer_recipe_1_to_2,
        description="Connecting op-node-1 to op-node-2",
    )

    plan.request(
        service_name="op-cl-2151908-3-op-node-custom-op-kurtosis",
        recipe=connect_peer_recipe_3_to_1,
        description="Connecting op-node-3 to op-node-1",
    )

    plan.request(
        service_name="op-cl-2151908-1-op-node-op-geth-op-kurtosis",
        recipe=connect_peer_recipe_1_to_3,
        description="Connecting op-node-1 to op-node-3",
    )

    tx_proxy_http_url = tx_proxy.launch(
        plan,
        service_name="tx-proxy",
        image="ghcr.io/worldcoin/tx-proxy:sha-9cdbe54",
        builder_rpc_0=builder_rpc_url,
        builder_rpc_1=builder_1_rpc_url,  # need to be separate client to prevent validation errors
        builder_rpc_2=builder_2_rpc_url,
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
