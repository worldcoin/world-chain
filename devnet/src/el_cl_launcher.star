ethereum_package_shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

ethereum_package_input_parser = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/input_parser.star"
)

input_parser = import_module("./package_io/input_parser.star")

observability = import_module("./observability/observability.star")

# EL
op_geth = import_module("./el/op-geth/op_geth_launcher.star")
op_reth = import_module("./el/op-reth/op_reth_launcher.star")
op_erigon = import_module("./el/op-erigon/op_erigon_launcher.star")
op_nethermind = import_module("./el/op-nethermind/op_nethermind_launcher.star")
op_besu = import_module("./el/op-besu/op_besu_launcher.star")
# CL
op_node = import_module("./cl/op-node/op_node_launcher.star")
hildr = import_module("./cl/hildr/hildr_launcher.star")

# MEV
rollup_boost = import_module("./mev/rollup-boost/rollup_boost_launcher.star")
op_geth_builder = import_module("./el/op-geth/op_geth_builder_launcher.star")
op_reth_builder = import_module("./el/op-reth/op_reth_builder_launcher.star")
world_chain_builder = import_module(
    "./el/op-reth/world_chain_builder_launcher.star"
)
op_node_builder = import_module("./cl/op-node/op_node_builder_launcher.star")


def launch(
    plan,
    jwt_file,
    network_params,
    mev_params,
    deployment_output,
    participants,
    num_participants,
    l1_config_env_vars,
    l2_services_suffix,
    global_log_level,
    global_node_selectors,
    global_tolerations,
    persistent,
    additional_services,
    observability_helper,
    interop_params,
    da_server_context,
):
    el_launchers = {
        "op-geth": {
            "launcher": op_geth.new_op_geth_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_geth.launch,
        },
        "op-reth": {
            "launcher": op_reth.new_op_reth_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_reth.launch,
        },
        "op-erigon": {
            "launcher": op_erigon.new_op_erigon_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_erigon.launch,
        },
        "op-nethermind": {
            "launcher": op_nethermind.new_nethermind_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_nethermind.launch,
        },
        "op-besu": {
            "launcher": op_besu.new_op_besu_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_besu.launch,
        },
    }

    el_builder_launchers = {
        "op-geth": {
            "launcher": op_geth_builder.new_op_geth_builder_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_geth_builder.launch,
        },
        "op-reth": {
            "launcher": op_reth_builder.new_op_reth_builder_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_reth_builder.launch,
        },
        "world-chain-builder": {
            "launcher": world_chain_builder.new_op_reth_builder_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": world_chain_builder.launch,
        },
    }

    cl_launchers = {
        "op-node": {
            "launcher": op_node.new_op_node_launcher(
                deployment_output, jwt_file, network_params
            ),
            "launch_method": op_node.launch,
        },
        "hildr": {
            "launcher": hildr.new_hildr_launcher(
                deployment_output, jwt_file, network_params
            ),
            "launch_method": hildr.launch,
        },
    }

    cl_builder_launchers = {
        "op-node": {
            "launcher": op_node_builder.new_op_node_builder_launcher(
                deployment_output, jwt_file, network_params
            ),
            "launch_method": op_node_builder.launch,
        },
    }

    sidecar_launchers = {
        "rollup-boost": {
            "launcher": rollup_boost.new_rollup_boost_launcher(
                deployment_output,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": rollup_boost.launch,
        }
    }

    all_cl_contexts = []
    all_el_contexts = []
    sequencer_enabled = True
    rollup_boost_enabled = "rollup-boost" in additional_services

    for index, participant in enumerate(participants):
        cl_type = participant.cl_type
        el_type = participant.el_type
        cl_builder_type = participant.cl_builder_type
        el_builder_type = participant.el_builder_type

        node_selectors = ethereum_package_input_parser.get_client_node_selectors(
            participant.node_selectors,
            global_node_selectors,
        )

        el_tolerations = ethereum_package_input_parser.get_client_tolerations(
            participant.el_tolerations, participant.tolerations, global_tolerations
        )

        cl_tolerations = ethereum_package_input_parser.get_client_tolerations(
            participant.cl_tolerations, participant.tolerations, global_tolerations
        )

        if el_type not in el_launchers:
            fail(
                "Unsupported launcher '{0}', need one of '{1}'".format(
                    el_type, ",".join(el_launchers.keys())
                )
            )
        if cl_type not in cl_launchers:
            fail(
                "Unsupported launcher '{0}', need one of '{1}'".format(
                    cl_type, ",".join(cl_launchers.keys())
                )
            )

        if el_builder_type not in el_builder_launchers:
            fail(
                "Unsupported launcher '{0}', need one of '{1}'".format(
                    el_builder_type, ",".join(el_builder_launchers.keys())
                )
            )

        if cl_builder_type not in cl_builder_launchers:
            fail(
                "Unsupported launcher '{0}', need one of '{1}'".format(
                    cl_builder_type, ",".join(cl_builder_launchers.keys())
                )
            )

        el_launcher, el_launch_method = (
            el_launchers[el_type]["launcher"],
            el_launchers[el_type]["launch_method"],
        )

        cl_launcher, cl_launch_method = (
            cl_launchers[cl_type]["launcher"],
            cl_launchers[cl_type]["launch_method"],
        )

        el_builder_launcher, el_builder_launch_method = (
            el_builder_launchers[el_builder_type]["launcher"],
            el_builder_launchers[el_builder_type]["launch_method"],
        )

        cl_builder_launcher, cl_builder_launch_method = (
            cl_builder_launchers[cl_builder_type]["launcher"],
            cl_builder_launchers[cl_builder_type]["launch_method"],
        )

        sidecar_launcher, sidecar_launch_method = (
            sidecar_launchers["rollup-boost"]["launcher"],
            sidecar_launchers["rollup-boost"]["launch_method"],
        )

        # Zero-pad the index using the calculated zfill value
        index_str = ethereum_package_shared_utils.zfill_custom(
            index + 1, len(str(len(participants)))
        )

        el_service_name = "op-el-{0}-{1}-{2}-{3}".format(
            index_str, el_type, cl_type, l2_services_suffix
        )
        cl_service_name = "op-cl-{0}-{1}-{2}-{3}".format(
            index_str, cl_type, el_type, l2_services_suffix
        )
        el_builder_service_name = "op-el-builder-{0}-{1}-{2}-{3}".format(
            index_str, el_builder_type, cl_builder_type, l2_services_suffix
        )
        cl_builder_service_name = "op-cl-builder-{0}-{1}-{2}-{3}".format(
            index_str, cl_builder_type, el_builder_type, l2_services_suffix
        )
        sidecar_service_name = "op-rollup-boost-{0}-{1}".format(
            index_str, l2_services_suffix
        )

        sequencer_context = all_el_contexts[0] if len(all_el_contexts) > 0 else None
        el_context = el_launch_method(
            plan,
            el_launcher,
            el_service_name,
            participant,
            global_log_level,
            persistent,
            el_tolerations,
            node_selectors,
            all_el_contexts,
            sequencer_enabled,
            sequencer_context,
            observability_helper,
            interop_params,
        )

        for metrics_info in [x for x in el_context.el_metrics_info if x != None]:
            observability.register_node_metrics_job(
                observability_helper, el_context.client_name, "execution", metrics_info
            )

        if rollup_boost_enabled and sequencer_enabled:
            plan.print("Starting rollup boost")

            if mev_params.builder_host == "" or mev_params.builder_port == "":
                el_builder_context = el_builder_launch_method(
                    plan,
                    el_builder_launcher,
                    el_builder_service_name,
                    participant,
                    global_log_level,
                    persistent,
                    el_tolerations,
                    node_selectors,
                    all_el_contexts,
                    sequencer_enabled,
                    sequencer_context,
                    observability_helper,
                    interop_params,
                )
            else:
                el_builder_context = struct(
                    ip_addr=mev_params.builder_host,
                    engine_rpc_port_num=mev_params.builder_port,
                    rpc_port_num=mev_params.builder_port,
                    rpc_http_url="http://{0}:{1}".format(
                        mev_params.builder_host, mev_params.builder_port
                    ),
                    client_name="external-builder",
                )

            rollup_boost_image = (
                mev_params.rollup_boost_image
                if mev_params.rollup_boost_image != ""
                else input_parser.DEFAULT_SIDECAR_IMAGES["rollup-boost"]
            )

            sidecar_context = sidecar_launch_method(
                plan,
                sidecar_launcher,
                sidecar_service_name,
                rollup_boost_image,
                all_el_contexts,
                el_context,
                el_builder_context,
            )

            all_el_contexts.append(el_builder_context)
        else:
            sidecar_context = None

        cl_context = cl_launch_method(
            plan,
            cl_launcher,
            cl_service_name,
            participant,
            global_log_level,
            persistent,
            cl_tolerations,
            node_selectors,
            sidecar_context
            if rollup_boost_enabled and sequencer_enabled
            else el_context,
            all_cl_contexts,
            l1_config_env_vars,
            sequencer_enabled,
            observability_helper,
            interop_params,
            da_server_context,
        )

        for metrics_info in [x for x in cl_context.cl_nodes_metrics_info if x != None]:
            observability.register_node_metrics_job(
                observability_helper,
                cl_context.client_name,
                "beacon",
                metrics_info,
                {
                    "supernode": str(cl_context.supernode),
                },
            )

        sequencer_enabled = False

        all_el_contexts.append(el_context)
        all_cl_contexts.append(cl_context)

        if rollup_boost_enabled and sequencer_enabled:
            cl_builder_context = cl_builder_launch_method(
                plan,
                cl_builder_launcher,
                cl_builder_service_name,
                participant,
                global_log_level,
                persistent,
                cl_tolerations,
                node_selectors,
                el_builder_context,
                all_cl_contexts,
                l1_config_env_vars,
                False,
                observability_helper,
                interop_params,
            )
            all_cl_contexts.append(cl_builder_context)

    plan.print("Successfully added {0} EL/CL participants".format(num_participants))
    return all_el_contexts, all_cl_contexts
