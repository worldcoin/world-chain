constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/constants.star"
)
shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)
# EL
op_geth = import_module("./el/op-geth/op_geth_launcher.star")
op_geth_builder = import_module("./el/op-geth-builder/op_geth_builder_launcher.star")
op_reth = import_module("./el/op-reth/op_reth_launcher.star")
world_chain = import_module("./el/world-chain/world_chain_launcher.star")
op_erigon = import_module("./el/op-erigon/op_erigon_launcher.star")
op_nethermind = import_module("./el/op-nethermind/op_nethermind_launcher.star")
op_besu = import_module("./el/op-besu/op_besu_launcher.star")
# CL
op_node = import_module("./cl/op-node/op_node_launcher.star")
hildr = import_module("./cl/hildr/hildr_launcher.star")


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
        "op-geth-builder": {
            "launcher": op_geth_builder.new_op_geth_builder_launcher(
                el_cl_data,
                jwt_file,
                network_params.network,
                network_params.network_id,
            ),
            "launch_method": op_geth_builder.launch,
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

    cl_launchers = {
        "op-node": {
            "launcher": op_node.new_op_node_launcher(
                el_cl_data, jwt_file, network_params
            ),
            "launch_method": op_node.launch,
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
        index_str = shared_utils.zfill_custom(index + 1, len(str(len(participants))))

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
