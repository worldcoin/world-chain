el_cl_client_launcher = import_module("./el_cl_launcher.star")
participant_module = import_module("./participant.star")
input_parser = import_module("./package_io/input_parser.star")
op_batcher_launcher = import_module("./batcher/op-batcher/op_batcher_launcher.star")
op_proposer_launcher = import_module(
    "github.com/ethpandaops/optimism-package/src/proposer/op-proposer/op_proposer_launcher.star@914a80895376625c8866943b517df47ce4c28170"
)
rundler_launcher = import_module("./bundler/rundler/rundler_launcher.star")

def launch_participant_network(
    plan,
    participants,
    jwt_file,
    entrypoint_config_file,
    mempool_config_file,
    chain_spec,
    network_params,
    el_cl_data,
    gs_private_keys,
    l1_config_env_vars,
    l2oo_address,
    l2_services_suffix,
):
    num_participants = len(participants)

    # First EL and sequencer CL
    all_el_contexts, all_cl_contexts = el_cl_client_launcher.launch(
        plan,
        jwt_file,
        network_params,
        el_cl_data,
        participants,
        num_participants,
        l1_config_env_vars,
        gs_private_keys["GS_SEQUENCER_PRIVATE_KEY"],
        l2_services_suffix,
    )

    all_participants = []
    for index, participant in enumerate(participants):
        el_type = participant.el_type
        cl_type = participant.cl_type

        el_context = all_el_contexts[index]
        cl_context = all_cl_contexts[index]

        participant_entry = participant_module.new_participant(
            el_type,
            cl_type,
            el_context,
            cl_context,
        )

        all_participants.append(participant_entry)

    op_batcher_launcher.launch(
        plan,
        "op-batcher{0}".format(l2_services_suffix),
        input_parser.DEFAULT_BATCHER_IMAGES["op-batcher"],
        all_el_contexts[0],
        all_cl_contexts[0],
        l1_config_env_vars,
        gs_private_keys["GS_BATCHER_PRIVATE_KEY"],
    )

    op_proposer_launcher.launch(
        plan,
        "op-proposer{0}".format(l2_services_suffix),
        input_parser.DEFAULT_PROPOSER_IMAGES["op-proposer"],
        all_cl_contexts[0],
        l1_config_env_vars,
        gs_private_keys["GS_PROPOSER_PRIVATE_KEY"],
        l2oo_address,
    )

    # Launch Rundler
    plan.print("Launching Rundler")
    rundler_launcher.launch(
        plan,
        "rundler{0}".format(l2_services_suffix),
        input_parser.DEFAULT_BUNDLER_IMAGES["rundler"],
        all_el_contexts[0],
        entrypoint_config_file,
        mempool_config_file,
        chain_spec,
    )
    
    return all_participants
