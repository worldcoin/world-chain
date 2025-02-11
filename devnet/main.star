ethereum_package = import_module("github.com/ethpandaops/ethereum-package/main.star")
contract_deployer = import_module("./src/contracts/contract_deployer.star")
l2_launcher = import_module("./src/l2.star")
op_supervisor_launcher = import_module(
    "./src/interop/op-supervisor/op_supervisor_launcher.star"
)
op_challenger_launcher = import_module(
    "./src/challenger/op-challenger/op_challenger_launcher.star"
)

observability = import_module("./src/observability/observability.star")
prometheus = import_module("./src/observability/prometheus/prometheus_launcher.star")
grafana = import_module("./src/observability/grafana/grafana_launcher.star")

wait_for_sync = import_module("./src/wait/wait_for_sync.star")
input_parser = import_module("./src/package_io/input_parser.star")
ethereum_package_static_files = import_module(
    "github.com/ethpandaops/ethereum-package/src/static_files/static_files.star"
)
rundler_static = import_module("./src/static/static_files.star")


def run(plan, args):
    """Deploy Optimism L2s on an Ethereum L1.

    Args:
        args(json): Configures other aspects of the environment.
    Returns:
        A full deployment of Optimism L2(s)
    """
    plan.print("Parsing the L1 input args")
    # If no args are provided, use the default values with minimal preset
    ethereum_args = args.get("ethereum_package", {})
    external_l1_args = args.get("external_l1_network_params", {})
    if external_l1_args:
        external_l1_args = input_parser.external_l1_network_params_input_parser(
            plan, external_l1_args
        )
    else:
        if "network_params" not in ethereum_args:
            ethereum_args.update(input_parser.default_ethereum_package_network_params())

    # need to do a raw get here in case only optimism_package is provided.
    # .get will return None if the key is in the config with a None value.
    optimism_args = args.get("optimism_package") or input_parser.default_optimism_args()
    optimism_args_with_right_defaults = input_parser.input_parser(plan, optimism_args)
    global_tolerations = optimism_args_with_right_defaults.global_tolerations
    global_node_selectors = optimism_args_with_right_defaults.global_node_selectors
    global_log_level = optimism_args_with_right_defaults.global_log_level
    persistent = optimism_args_with_right_defaults.persistent
    altda_deploy_config = optimism_args_with_right_defaults.altda_deploy_config

    observability_params = optimism_args_with_right_defaults.observability
    interop_params = optimism_args_with_right_defaults.interop

    observability_helper = observability.make_helper(observability_params)

    # Deploy the L1
    l1_network = ""
    if external_l1_args:
        plan.print("Using external L1")
        plan.print(external_l1_args)

        l1_rpc_url = external_l1_args.el_rpc_url
        l1_priv_key = external_l1_args.priv_key

        l1_config_env_vars = {
            "L1_RPC_KIND": external_l1_args.rpc_kind,
            "L1_RPC_URL": l1_rpc_url,
            "CL_RPC_URL": external_l1_args.cl_rpc_url,
            "L1_WS_URL": external_l1_args.el_ws_url,
            "L1_CHAIN_ID": external_l1_args.network_id,
        }

        plan.print("Waiting for network to sync")
        wait_for_sync.wait_for_sync(plan, l1_config_env_vars)
    else:
        plan.print("Deploying a local L1")
        l1 = ethereum_package.run(plan, ethereum_args)
        plan.print(l1.network_params)
        # Get L1 info
        all_l1_participants = l1.all_participants
        l1_network = "local"
        l1_network_params = l1.network_params
        l1_network_id = l1.network_id
        l1_rpc_url = all_l1_participants[0].el_context.rpc_http_url
        l1_priv_key = l1.pre_funded_accounts[
            12
        ].private_key  # reserved for L2 contract deployers
        l1_config_env_vars = get_l1_config(
            all_l1_participants, l1_network_params, l1_network_id
        )
        plan.print("Waiting for L1 to start up")
        wait_for_sync.wait_for_startup(plan, l1_config_env_vars)

    deployment_output = contract_deployer.deploy_contracts(
        plan,
        l1_priv_key,
        l1_config_env_vars,
        optimism_args_with_right_defaults,
        l1_network,
        altda_deploy_config,
    )

    jwt_file = plan.upload_files(
        src=ethereum_package_static_files.JWT_PATH_FILEPATH,
        name="op_jwt_file",
    )

    entrypoint_config_file = plan.upload_files(
        src=rundler_static.ENTRYPOINT_CONFIG_FILE_PATH,
        name="entrypoint_config.json",
    )

    mempool_config_file = plan.upload_files(
        src=rundler_static.MEMPOOL_CONFIG_FILE_PATH,
        name="mempool_config.json",
    )

    rundler_chain_spec = plan.upload_files(
        src=rundler_static.RUNDLER_CHAIN_SPEC_FILE_PATH,
        name="chain_spec.json",
    )

    l2s = []
    for l2_num, chain in enumerate(optimism_args_with_right_defaults.chains):
        l2s.append(
            l2_launcher.launch_l2(
                plan,
                l2_num,
                chain.network_params.name,
                chain,
                jwt_file,
                deployment_output,
                l1_config_env_vars,
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
                rundler_chain_spec,
            )
        )

    if interop_params.enabled:
        op_supervisor_launcher.launch(
            plan,
            l1_config_env_vars,
            optimism_args_with_right_defaults.chains,
            l2s,
            jwt_file,
            interop_params.supervisor_params,
            observability_helper,
        )

    # challenger must launch after supervisor because it depends on it for interop
    for l2_num, l2 in enumerate(l2s):
        chain = optimism_args_with_right_defaults.chains[l2_num]
        op_challenger_image = (
            chain.challenger_params.image
            if chain.challenger_params.image != ""
            else input_parser.DEFAULT_CHALLENGER_IMAGES["op-challenger"]
        )
        op_challenger_launcher.launch(
            plan,
            l2_num,
            "op-challenger-{0}".format(chain.network_params.name),
            chain.challenger_params.image,
            l2.participants[0].el_context,
            l2.participants[0].cl_context,
            l1_config_env_vars,
            deployment_output,
            chain.network_params,
            chain.challenger_params,
            interop_params,
            observability_helper,
        )

    if observability_helper.enabled and len(observability_helper.metrics_jobs) > 0:
        plan.print("Launching prometheus...")
        prometheus_private_url = prometheus.launch_prometheus(
            plan,
            observability_helper,
            global_node_selectors,
        )

        plan.print("Launching grafana...")
        grafana.launch_grafana(
            plan,
            prometheus_private_url,
            global_node_selectors,
            observability_params.grafana_params,
        )


def get_l1_config(all_l1_participants, l1_network_params, l1_network_id):
    env_vars = {}
    env_vars["L1_RPC_KIND"] = "standard"
    env_vars["WEB3_RPC_URL"] = str(all_l1_participants[0].el_context.rpc_http_url)
    env_vars["L1_RPC_URL"] = str(all_l1_participants[0].el_context.rpc_http_url)
    env_vars["CL_RPC_URL"] = str(all_l1_participants[0].cl_context.beacon_http_url)
    env_vars["L1_WS_URL"] = str(all_l1_participants[0].el_context.ws_url)
    env_vars["L1_CHAIN_ID"] = str(l1_network_id)
    env_vars["L1_BLOCK_TIME"] = str(l1_network_params.seconds_per_slot)
    return env_vars
