OBSERVABILITY_PARAMS = [
    "enabled",
    "prometheus_params",
    "grafana_params",
]

PROMETHEUS_PARAMS = [
    "image",
    "storage_tsdb_retention_time",
    "storage_tsdb_retention_size",
    "min_cpu",
    "max_cpu",
    "min_mem",
    "max_mem",
]

GRAFANA_PARAMS = [
    "image",
    "dashboard_sources",
    "min_cpu",
    "max_cpu",
    "min_mem",
    "max_mem",
]

INTEROP_PARAMS = [
    "enabled",
    "supervisor_params",
]

SUPERVISOR_PARAMS = [
    "image",
    "dependency_set",
    "extra_params",
]

ALTDA_DEPLOY_CONFIG_PARAMS = [
    "use_altda",
    "da_commitment_type",
    "da_challenge_window",
    "da_resolve_window",
    "da_bond_size",
    "da_resolver_refund_percentage",
]

PARTICIPANT_CATEGORIES = {
    "participants": [
        "el_type",
        "el_image",
        "el_log_level",
        "el_extra_env_vars",
        "el_extra_labels",
        "el_extra_params",
        "el_tolerations",
        "el_volume_size",
        "el_min_cpu",
        "el_max_cpu",
        "el_min_mem",
        "el_max_mem",
        "cl_type",
        "cl_image",
        "cl_log_level",
        "cl_extra_env_vars",
        "cl_extra_labels",
        "cl_extra_params",
        "cl_tolerations",
        "cl_volume_size",
        "cl_min_cpu",
        "cl_max_cpu",
        "cl_min_mem",
        "cl_max_mem",
        "el_builder_type",
        "el_builder_image",
        "cl_builder_type",
        "cl_builder_image",
        "node_selectors",
        "tolerations",
        "count",
    ],
}

SUBCATEGORY_PARAMS = {
    "network_params": [
        "network",
        "network_id",
        "seconds_per_slot",
        "name",
        "fjord_time_offset",
        "granite_time_offset",
        "holocene_time_offset",
        "isthmus_time_offset",
        "interop_time_offset",
        "fund_dev_accounts",
    ],
    "batcher_params": ["image", "extra_params"],
    "challenger_params": [
        "image",
        "extra_params",
        "cannon_prestate_path",
        "cannon_prestates_url",
        "cannon_trace_types",
    ],
    "proposer_params": ["image", "extra_params", "game_type", "proposal_interval"],
    "mev_params": ["rollup_boost_image", "builder_host", "builder_port"],
    "da_server_params": [
        "image",
        "cmd",
    ],
}

OP_CONTRACT_DEPLOYER_PARAMS = [
    "image",
    "l1_artifacts_locator",
    "l2_artifacts_locator",
    "global_deploy_overrides",
]

OP_CONTRACT_DEPLOYER_GLOBAL_DEPLOY_OVERRIDES = ["faultGameAbsolutePrestate"]

ADDITIONAL_SERVICES_PARAMS = [
    "blockscout",
    "rollup-boost",
    "da_server",
]

ROOT_PARAMS = [
    "observability",
    "interop",
    "altda_deploy_config",
    "chains",
    "op_contract_deployer_params",
    "global_log_level",
    "global_node_selectors",
    "global_tolerations",
    "persistent",
]

EXTERNAL_L1_NETWORK_PARAMS = [
    "network_id",
    "rpc_kind",
    "el_rpc_url",
    "el_ws_url",
    "cl_rpc_url",
    "priv_key",
]


def deep_validate_params(plan, input_args, category, allowed_params):
    if category in input_args:
        for item in input_args[category]:
            for param in item.keys():
                if param not in allowed_params:
                    fail(
                        "Invalid parameter {0} for {1}. Allowed fields: {2}".format(
                            param, category, allowed_params
                        )
                    )


def validate_params(plan, input_args, category, allowed_params):
    if category in input_args:
        for param in input_args[category].keys():
            if param not in allowed_params:
                fail(
                    "Invalid parameter {0} for {1}. Allowed fields: {2}".format(
                        param, category, allowed_params
                    )
                )


def sanity_check(plan, optimism_config):
    if type(optimism_config) != "dict":
        fail("Invalid input_args type, expected dict")

    for key in optimism_config.keys():
        if key not in ROOT_PARAMS:
            fail("Invalid parameter {0}, allowed fields: {1}".format(key, ROOT_PARAMS))

    if "observability" in optimism_config:
        validate_params(
            plan,
            optimism_config,
            "observability",
            OBSERVABILITY_PARAMS,
        )

        if "prometheus_params" in optimism_config["observability"]:
            validate_params(
                plan,
                optimism_config["observability"],
                "prometheus_params",
                PROMETHEUS_PARAMS,
            )

        if "grafana_params" in optimism_config["observability"]:
            validate_params(
                plan,
                optimism_config["observability"],
                "grafana_params",
                GRAFANA_PARAMS,
            )

    if "interop" in optimism_config:
        validate_params(
            plan,
            optimism_config,
            "interop",
            INTEROP_PARAMS,
        )

        if "supervisor_params" in optimism_config["interop"]:
            validate_params(
                plan,
                optimism_config["interop"],
                "supervisor_params",
                SUPERVISOR_PARAMS,
            )

    if "altda_deploy_config" in optimism_config:
        validate_params(
            plan,
            optimism_config["altda_deploy_config"],
            "altda_deploy_config",
            ALTDA_DEPLOY_CONFIG_PARAMS,
        )

    chains = optimism_config.get("chains", [])

    if type(chains) != "list":
        fail("Invalid input_args type, expected list")

    for input_args in chains:
        # Checks participants
        deep_validate_params(
            plan, input_args, "participants", PARTICIPANT_CATEGORIES["participants"]
        )

        # Checks additional_services
        if "additional_services" in input_args:
            for additional_services in input_args["additional_services"]:
                if additional_services not in ADDITIONAL_SERVICES_PARAMS:
                    fail(
                        "Invalid additional_services {0}, allowed fields: {1}".format(
                            additional_services, ADDITIONAL_SERVICES_PARAMS
                        )
                    )

        # Checks subcategories
        for subcategories in SUBCATEGORY_PARAMS.keys():
            validate_params(
                plan, input_args, subcategories, SUBCATEGORY_PARAMS[subcategories]
            )
        # Checks everything else
        for param in input_args.keys():
            combined_root_params = (
                PARTICIPANT_CATEGORIES.keys() + SUBCATEGORY_PARAMS.keys()
            )
            combined_root_params.append("additional_services")
            combined_root_params.append("op_contract_deployer_params")
            combined_root_params.append("supervisor_params")

            if param not in combined_root_params:
                fail(
                    "Invalid parameter {0}, allowed fields {1}".format(
                        param, combined_root_params
                    )
                )

        # If everything passes, print a message

    if "op_contract_deployer_params" in optimism_config:
        validate_params(
            plan,
            optimism_config,
            "op_contract_deployer_params",
            OP_CONTRACT_DEPLOYER_PARAMS,
        )
        validate_params(
            plan,
            optimism_config["op_contract_deployer_params"],
            "global_deploy_overrides",
            OP_CONTRACT_DEPLOYER_GLOBAL_DEPLOY_OVERRIDES,
        )

    plan.print("Sanity check for OP package passed")


def external_l1_network_params_input_parser(plan, external_l1_network_params):
    for key in external_l1_network_params.keys():
        if key not in EXTERNAL_L1_NETWORK_PARAMS:
            fail(
                "Invalid parameter {0}, allowed fields: {1}".format(
                    key, EXTERNAL_L1_NETWORK_PARAMS
                )
            )
