constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/constants.star"
)
shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

# EL
op_reth = import_module(
    "github.com/ethpandaops/optimism-package/src/el/op-reth/op_reth_launcher.star@914a80895376625c8866943b517df47ce4c28170"
)
op_erigon = import_module(
    "github.com/ethpandaops/optimism-package/src/el/op-erigon/op_erigon_launcher.star@914a80895376625c8866943b517df47ce4c28170"
)
op_nethermind = import_module(
    "github.com/ethpandaops/optimism-package/src/el/op-nethermind/op_nethermind_launcher.star"
)
op_besu = import_module(
    "github.com/ethpandaops/optimism-package/src/el/op-besu/op_besu_launcher.star"
)
op_geth = import_module("./el/op-geth/op_geth_launcher.star")
world_chain = import_module("./el/world-chain/world_chain_launcher.star")
rollup_boost = import_module("./engine/rollup-boost/rollup_boost_launcher.star")

# CL
op_node = import_module("./cl/op-node/op_node_launcher.star")
op_node_builder = import_module("./cl/op-node-builder/op_node_builder_launcher.star")
hildr = import_module(
    "github.com/ethpandaops/optimism-package/src/cl/hildr/hildr_launcher.star"
)


def launch(
    plan,
    jwt_file,
    network_params,
    el_cl_data,
    participants,
    num_participants,
    l1_config_env_vars,
    gs_sequencer_private_key,
    l2_services_suffix,
):
    el_launchers = {
        "world-chain": {
            "launcher": world_chain.new_world_chain_launcher(
                el_cl_data,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": world_chain.launch,
        },
        "op-geth": {
            "launcher": op_geth.new_op_geth_launcher(
                el_cl_data,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_geth.launch,
        },
        "op-reth": {
            "launcher": op_reth.new_op_reth_launcher(
                el_cl_data,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_reth.launch,
        },
        "op-erigon": {
            "launcher": op_erigon.new_op_erigon_launcher(
                el_cl_data,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_erigon.launch,
        },
        "op-nethermind": {
            "launcher": op_nethermind.new_nethermind_launcher(
                el_cl_data,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_nethermind.launch,
        },
        "op-besu": {
            "launcher": op_besu.new_op_besu_launcher(
                el_cl_data,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_besu.launch,
        },
    }

    engine_relay_launchers = {
        "rollup-boost": {
            "launcher": rollup_boost.new_rollup_boost_launcher(
                el_cl_data,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": rollup_boost.launch,
        }
    }

    cl_launchers = {
        "op-node": {
            "launcher": op_node.new_op_node_launcher(
                el_cl_data, jwt_file, network_params
            ),
            "launch_method": op_node.launch,
        },
        "op-node-builder": {
            "launcher": op_node_builder.new_op_node_launcher(
                el_cl_data, jwt_file, network_params
            ),
            "launch_method": op_node_builder.launch,
        },
        "hildr": {
            "launcher": hildr.new_hildr_launcher(el_cl_data, jwt_file, network_params),
            "launch_method": hildr.launch,
        },
    }

    all_cl_contexts = []
    all_el_contexts = []
    sequencer_enabled = True
    for index, participant in enumerate(participants):
        if participant.admin:
            # op-geth EL and op-node CL
            el_type = participant.el_type
            cl_type = participant.cl_type

            # world-chain-builder EL & op-node CL
            el_builder_type = participant.el_builder_type
            cl_builder_type = participant.cl_builder_type

            # engine relay
            engine_relay_type = participant.engine_relay_type

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

            if el_builder_type not in el_launchers:
                fail(
                    "Unsupported launcher '{0}', need one of '{1}'".format(
                        el_builder_type, ",".join(el_launchers.keys())
                    )
                )

            if cl_builder_type not in cl_launchers:
                fail(
                    "Unsupported launcher '{0}', need one of '{1}'".format(
                        cl_builder_type, ",".join(cl_launchers.keys())
                    )
                )

            if engine_relay_type not in engine_relay_launchers:
                fail(
                    "Unsupported launcher '{0}', need one of '{1}'".format(
                        engine_relay_type, ",".join(engine_relay_launchers.keys())
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
                el_launchers[el_builder_type]["launcher"],
                el_launchers[el_builder_type]["launch_method"],
            )

            cl_builder_launcher, cl_builder_launch_method = (
                cl_launchers[cl_builder_type]["launcher"],
                cl_launchers[cl_builder_type]["launch_method"],
            )

            engine_relay_launcher, engine_relay_launch_method = (
                engine_relay_launchers[engine_relay_type]["launcher"],
                engine_relay_launchers[engine_relay_type]["launch_method"],
            )

            # Zero-pad the index using the calculated zfill value
            index_str = shared_utils.zfill_custom(
                index + 1, len(str(len(participants)))
            )

            el_service_name = "wc-admin-{0}".format(el_type)

            cl_service_name = "wc-admin-{0}".format(cl_type)

            el_builder_service_name = "wc-admin-{0}-builder".format(el_builder_type)

            cl_builder_service_name = "wc-admin-{0}-builder".format(cl_builder_type)

            engine_relayer_service_name = "wc-admin-{0}-engine".format(
                engine_relay_type
            )

            # First launch the Sequencer, and the Builder
            el_builder_context = el_builder_launch_method(
                plan,
                el_builder_launcher,
                el_builder_service_name,
                participant.el_builder_image,
                all_el_contexts,
                False,  # sequencer disabled
                None,  # sequencer context
            )

            el_context = el_launch_method(
                plan,
                el_launcher,
                el_service_name,
                participant.el_image,
                all_el_contexts,
                True,  # sequencer enabled
                None,  # sequencer context
            )

            # Launch the Engine Relay w/ engine rpc on el_context/el_builder_context
            engine_relay_context = engine_relay_launch_method(
                plan,
                engine_relay_launcher,
                engine_relayer_service_name,
                participant.engine_relay_image,
                all_el_contexts,
                el_context,
                el_builder_context,
            )

            # Launch op-node pointing to the Engine Relay
            cl_context = cl_launch_method(
                plan,
                cl_launcher,
                cl_service_name,
                participant.cl_image,
                engine_relay_context,
                all_cl_contexts,
                l1_config_env_vars,
                gs_sequencer_private_key,
                sequencer_enabled,
            )

            # Launch the CL Builder
            # TODO: --p2p.static=/dns/<ipv4>/tcp/9003/p2p/
            cl_builder_context = cl_builder_launch_method(
                plan,
                cl_builder_launcher,
                cl_builder_service_name,
                participant.cl_builder_image,
                el_builder_context,
                all_cl_contexts,
                l1_config_env_vars,
                gs_sequencer_private_key,
                sequencer_enabled,
            )
            all_el_contexts.append(engine_relay_context)
            all_el_contexts.append(el_context)
            all_el_contexts.append(
                el_builder_context
            )  # [rollup-boost, op-geth, world-chain]
            all_cl_contexts.append(cl_context)
            all_cl_contexts.append(cl_builder_context)
        else:
            # Launch a standard participant (non sequencer)
            cl_type = participant.cl_type
            el_type = participant.el_type

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

            el_launcher, el_launch_method = (
                el_launchers[el_type]["launcher"],
                el_launchers[el_type]["launch_method"],
            )

            cl_launcher, cl_launch_method = (
                cl_launchers[cl_type]["launcher"],
                cl_launchers[cl_type]["launch_method"],
            )

            # Zero-pad the index using the calculated zfill value
            index_str = shared_utils.zfill_custom(
                index + 1, len(str(len(participants)))
            )

            el_service_name = "op-el-{0}-{1}-{2}{3}".format(
                index_str, el_type, cl_type, l2_services_suffix
            )
            cl_service_name = "op-cl-{0}-{1}-{2}{3}".format(
                index_str, cl_type, el_type, l2_services_suffix
            )

            el_context = el_launch_method(
                plan,
                el_launcher,
                el_service_name,
                participant.el_image,
                all_el_contexts,
                sequencer_enabled,
                all_cl_contexts[0]
                if len(all_cl_contexts) > 0
                else None,  # sequencer context
            )

            cl_context = cl_launch_method(
                plan,
                cl_launcher,
                cl_service_name,
                participant.cl_image,
                el_context,
                all_cl_contexts,
                l1_config_env_vars,
                gs_sequencer_private_key,
                sequencer_enabled,
            )

            sequencer_enabled = False

            all_el_contexts.append(el_context)
            all_cl_contexts.append(cl_context)

    plan.print("Successfully added {0} EL/CL participants".format(num_participants))
    return all_el_contexts, all_cl_contexts
