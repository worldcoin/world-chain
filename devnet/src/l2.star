participant_network = import_module("./participant_network.star")
blockscout = import_module("./blockscout/blockscout_launcher.star")
da_server_launcher = import_module("./alt-da/da-server/da_server_launcher.star")
contract_deployer = import_module("./contracts/contract_deployer.star")
input_parser = import_module("./package_io/input_parser.star")
util = import_module("./util.star")


def launch_l2(
    plan,
    l2_num,
    l2_services_suffix,
    l2_args,
    jwt_file,
    deployment_output,
    l1_config,
    l1_priv_key,
    l1_rpc_url,
    global_log_level,
    global_node_selectors,
    global_tolerations,
    persistent,
    observability_helper,
    interop_params,
    entrypoint_config_file,
    mempool_config_file,
    chain_spec,
):
    network_params = l2_args.network_params
    batcher_params = l2_args.batcher_params
    challenger_params = l2_args.challenger_params
    proposer_params = l2_args.proposer_params
    mev_params = l2_args.mev_params

    plan.print("Deploying L2 with name {0}".format(network_params.name))

    # we need to launch da-server before launching the participant network
    # because op-batcher and op-node(s) need to know the da-server url, if present
    da_server_context = da_server_launcher.disabled_da_server_context()
    if "da_server" in l2_args.additional_services:
        da_server_image = l2_args.da_server_params.image
        plan.print("Launching da-server")
        da_server_context = da_server_launcher.launch_da_server(
            plan,
            "da-server-{0}".format(l2_services_suffix),
            da_server_image,
            l2_args.da_server_params.cmd,
        )
        plan.print("Successfully launched da-server")

    l2 = participant_network.launch_participant_network(
        plan,
        l2_args.participants,
        jwt_file,
        network_params,
        batcher_params,
        challenger_params,
        proposer_params,
        mev_params,
        deployment_output,
        l1_config,
        l2_num,
        l2_services_suffix,
        global_log_level,
        global_node_selectors,
        global_tolerations,
        persistent,
        l2_args.additional_services,
        observability_helper,
        interop_params,
        da_server_context,
        entrypoint_config_file,
        mempool_config_file,
        chain_spec,
    )

    all_el_contexts = []
    all_cl_contexts = []
    for participant in l2.participants:
        all_el_contexts.append(participant.el_context)
        all_cl_contexts.append(participant.cl_context)

    network_id_as_hex = util.to_hex_chain_id(network_params.network_id)
    l1_bridge_address = util.read_network_config_value(
        plan,
        deployment_output,
        "state",
        '.opChainDeployments[] | select(.id=="{0}") | .l1StandardBridgeProxyAddress'.format(
            network_id_as_hex
        ),
    )

    for additional_service in l2_args.additional_services:
        if additional_service == "blockscout":
            plan.print("Launching op-blockscout")
            blockscout.launch_blockscout(
                plan,
                l2_services_suffix,
                l1_rpc_url,
                all_el_contexts[0],  # first l2 EL url
                network_params.name,
                deployment_output,
                network_params.network_id,
            )
            plan.print("Successfully launched op-blockscout")

    plan.print(l2.participants)
    plan.print(
        "Begin your L2 adventures by depositing some L1 Kurtosis ETH to: {0}".format(
            l1_bridge_address
        )
    )

    return l2
