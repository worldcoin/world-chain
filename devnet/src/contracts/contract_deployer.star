ENVRC_PATH = "/workspace/optimism/.envrc"
FACTORY_DEPLOYER_ADDRESS = "0x3fAB184622Dc19b6109349B94811493BF2a45362"
FACTORY_ADDRESS = "0x4e59b44847b379578588920cA78FbF26c0B4956C"
# raw tx data for deploying Create2Factory contract to L1
FACTORY_DEPLOYER_CODE = "0xf8a58085174876e800830186a08080b853604580600e600039806000f350fe7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe03601600081602082378035828234f58015156039578182fd5b8082525050506014600cf31ba02222222222222222222222222222222222222222222222222222222222222222a02222222222222222222222222222222222222222222222222222222222222222"

FUND_SCRIPT_FILEPATH = "../../static_files/scripts"

utils = import_module("../util.star")

ethereum_package_genesis_constants = import_module(
    "github.com/ethpandaops/ethereum-package/src/prelaunch_data_generator/genesis_constants/genesis_constants.star"
)

CANNED_VALUES = {
    "eip1559Denominator": 50,
    "eip1559DenominatorCanyon": 250,
    "eip1559Elasticity": 6,
}


def deploy_contracts(
    plan, priv_key, l1_config_env_vars, optimism_args, l1_network, altda_args
):
    l2_chain_ids_list = [
        str(chain.network_params.network_id) for chain in optimism_args.chains
    ]
    l2_chain_ids = ",".join(l2_chain_ids_list)

    op_deployer_init = plan.run_sh(
        name="op-deployer-init",
        description="Initialize L2 contract deployments",
        image=optimism_args.op_contract_deployer_params.image,
        env_vars=l1_config_env_vars,
        store=[
            StoreSpec(
                src="/network-data",
                name="op-deployer-configs",
            )
        ],
        run=" && ".join(
            [
                "mkdir -p /network-data",
                "op-deployer init --intent-config-type custom --l1-chain-id $L1_CHAIN_ID --l2-chain-ids {0} --workdir /network-data".format(
                    l2_chain_ids
                ),
            ]
        ),
    )

    fund_script_artifact = plan.upload_files(
        src=FUND_SCRIPT_FILEPATH,
        name="op-deployer-fund-script",
    )

    plan.run_sh(
        name="op-deployer-fund",
        description="Collect keys, and fund addresses",
        image=utils.DEPLOYMENT_UTILS_IMAGE,
        env_vars={
            "DEPLOYER_PRIVATE_KEY": priv_key,
            "FUND_PRIVATE_KEY": ethereum_package_genesis_constants.PRE_FUNDED_ACCOUNTS[
                19
            ].private_key,
            "FUND_VALUE": "10ether",
            "L1_NETWORK": str(l1_network),
        }
        | l1_config_env_vars,
        store=[
            StoreSpec(
                src="/network-data",
                name="op-deployer-configs",
            )
        ],
        files={
            "/network-data": op_deployer_init.files_artifacts[0],
            "/fund-script": fund_script_artifact,
        },
        run='bash /fund-script/fund.sh "{0}"'.format(l2_chain_ids),
    )

    hardfork_schedule = []
    for index, chain in enumerate(optimism_args.chains):
        np = chain.network_params

        # rename each hardfork to the name the override expects
        renames = (
            ("l2GenesisFjordTimeOffset", np.fjord_time_offset),
            ("l2GenesisGraniteTimeOffset", np.granite_time_offset),
            ("l2GenesisHoloceneTimeOffset", np.holocene_time_offset),
            ("l2GenesisIsthmusTimeOffset", np.isthmus_time_offset),
            ("l2GenesisInteropTimeOffset", np.interop_time_offset),
        )

        # only include the hardforks that have been activated since
        # toml does not support null values
        for fork_key, activation_timestamp in renames:
            if activation_timestamp != None:
                hardfork_schedule.append((index, fork_key, activation_timestamp))

    intent = {
        "useInterop": optimism_args.interop.enabled,
        "l1ContractsLocator": optimism_args.op_contract_deployer_params.l1_artifacts_locator,
        "l2ContractsLocator": optimism_args.op_contract_deployer_params.l2_artifacts_locator,
        "superchainRoles": {
            "guardian": read_chain_cmd("l1ProxyAdmin", l2_chain_ids_list[0]),
            "protocolVersionsOwner": read_chain_cmd(
                "l1ProxyAdmin", l2_chain_ids_list[0]
            ),
            "proxyAdminOwner": read_chain_cmd("l1ProxyAdmin", l2_chain_ids_list[0]),
        },
        "chains": [],
    }

    absolute_prestate = ""
    if optimism_args.op_contract_deployer_params.global_deploy_overrides[
        "faultGameAbsolutePrestate"
    ]:
        absolute_prestate = (
            optimism_args.op_contract_deployer_params.global_deploy_overrides[
                "faultGameAbsolutePrestate"
            ]
        )
        intent["globalDeployOverrides"] = {
            "dangerouslyAllowCustomDisputeParameters": True,
            "faultGameAbsolutePrestate": absolute_prestate,
        }

    for i, chain in enumerate(optimism_args.chains):
        chain_id = str(chain.network_params.network_id)
        intent_chain = dict(CANNED_VALUES)
        intent_chain.update(
            {
                "deployOverrides": {
                    "l2BlockTime": chain.network_params.seconds_per_slot,
                    "fundDevAccounts": True
                    if chain.network_params.fund_dev_accounts
                    else False,
                },
                "baseFeeVaultRecipient": read_chain_cmd(
                    "baseFeeVaultRecipient", chain_id
                ),
                "l1FeeVaultRecipient": read_chain_cmd("l1FeeVaultRecipient", chain_id),
                "sequencerFeeVaultRecipient": read_chain_cmd(
                    "sequencerFeeVaultRecipient", chain_id
                ),
                "roles": {
                    "batcher": read_chain_cmd("batcher", chain_id),
                    "challenger": read_chain_cmd("challenger", chain_id),
                    "l1ProxyAdminOwner": read_chain_cmd("l1ProxyAdmin", chain_id),
                    "l2ProxyAdminOwner": read_chain_cmd("l2ProxyAdmin", chain_id),
                    "proposer": read_chain_cmd("proposer", chain_id),
                    "systemConfigOwner": read_chain_cmd("systemConfigOwner", chain_id),
                    "unsafeBlockSigner": read_chain_cmd("sequencer", chain_id),
                },
                "dangerousAdditionalDisputeGames": [
                    {
                        "respectedGameType": 0,
                        "faultGameAbsolutePrestate": absolute_prestate,
                        "faultGameMaxDepth": 73,
                        "faultGameSplitDepth": 30,
                        "faultGameClockExtension": 10800,
                        "faultGameMaxClockDuration": 302400,
                        "dangerouslyAllowCustomDisputeParameters": True,
                        "vmType": "CANNON1",
                        "useCustomOracle": False,
                        "oracleMinProposalSize": 0,
                        "oracleChallengePeriodSeconds": 0,
                        "makeRespected": False,
                    }
                ],
                "dangerousAltDAConfig": {
                    "useAltDA": altda_args.use_altda,
                    "daCommitmentType": altda_args.da_commitment_type,
                    "daChallengeWindow": altda_args.da_challenge_window,
                    "daResolveWindow": altda_args.da_resolve_window,
                    "daBondSize": altda_args.da_bond_size,
                },
            }
        )
        for index, fork_key, activation_timestamp in hardfork_schedule:
            intent_chain["deployOverrides"][fork_key] = "0x%x" % activation_timestamp
        intent["chains"].append(intent_chain)

    intent_json = json.encode(intent)
    intent_json_artifact = utils.write_to_file(plan, intent_json, "/tmp", "intent.json")

    op_deployer_configure = plan.run_sh(
        name="op-deployer-configure",
        description="Configure L2 contract deployments",
        image=utils.DEPLOYMENT_UTILS_IMAGE,
        store=[
            StoreSpec(
                src="/network-data",
                name="op-deployer-configs",
            )
        ],
        files={
            "/network-data": op_deployer_init.files_artifacts[0],
            "/tmp": intent_json_artifact,
        },
        run=" && ".join(
            [
                # zhwrd: this mess is temporary until we implement json reading for op-deployer intent file
                # convert intent_json to yaml. this is necessary because its unreliable to evaluate command substitutions in json.
                """cat /tmp/intent.json | dasel -r json -w yaml > /network-data/intent.yaml""",
                # evaluate the command substitutions
                "eval \"echo '$(cat /network-data/intent.yaml)'\" | dasel -r yaml -w json > /network-data/intent-b.json",
                # convert op-deployer generated intent.toml to json
                "dasel -r toml -w json -f /network-data/intent.toml > /network-data/intent-a.json",
                # merge the two intent.json files, ensuring that the chains array is merged correctly
                "jq -s 'add + {chains: map(.chains) | transpose | map(add)}' /network-data/intent-a.json /network-data/intent-b.json > /network-data/intent-merged.json",
                # convert the merged intent.json back to toml
                "cat /network-data/intent-merged.json | dasel -r json -w toml > /network-data/intent.toml",
            ]
        ),
    )

    apply_cmds = [
        "op-deployer apply --l1-rpc-url $L1_RPC_URL --private-key $PRIVATE_KEY --workdir /network-data",
    ]
    for chain in optimism_args.chains:
        network_id = chain.network_params.network_id
        apply_cmds.extend(
            [
                "op-deployer inspect genesis --workdir /network-data --outfile /network-data/genesis-{0}.json {0}".format(
                    network_id
                ),
                "op-deployer inspect rollup --workdir /network-data --outfile /network-data/rollup-{0}.json {0}".format(
                    network_id
                ),
            ]
        )

    op_deployer_output = plan.run_sh(
        name="op-deployer-apply",
        description="Apply L2 contract deployments",
        image=optimism_args.op_contract_deployer_params.image,
        env_vars={"PRIVATE_KEY": str(priv_key)} | l1_config_env_vars,
        store=[
            StoreSpec(
                src="/network-data",
                name="op-deployer-configs",
            )
        ],
        files={
            "/network-data": op_deployer_configure.files_artifacts[0],
        },
        run=" && ".join(apply_cmds),
    )

    for chain in optimism_args.chains:
        plan.run_sh(
            name="op-deployer-generate-chainspec",
            description="Generate chainspec",
            image=utils.DEPLOYMENT_UTILS_IMAGE,
            env_vars={"CHAIN_ID": str(chain.network_params.network_id)},
            store=[
                StoreSpec(
                    src="/network-data",
                    name="op-deployer-configs",
                )
            ],
            files={
                "/network-data": op_deployer_output.files_artifacts[0],
                "/fund-script": fund_script_artifact,
            },
            run='jq --from-file /fund-script/gen2spec.jq < "/network-data/genesis-$CHAIN_ID.json" > "/network-data/chainspec-$CHAIN_ID.json"',
        )

    return op_deployer_output.files_artifacts[0]


def chain_key(index, key):
    return "chains.[{0}].{1}".format(index, key)


def read_chain_cmd(filename, l2_chain_id):
    return "`jq -r .address /network-data/{0}-{1}.json`".format(filename, l2_chain_id)
