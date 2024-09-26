ethereum_package_input_parser = import_module(
    "github.com/ethpandaops/ethereum-package/src/package_io/input_parser.star"
)

sanity_check = import_module("./sanity_check.star")

DEFAULT_EL_IMAGES = {
    "world-chain": "world-chain-builder:latest",
    "op-geth-builder": "docker.io/flashbots/op-geth:0.11014080.2",
    "op-geth": "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-geth:latest",
    "op-reth": "ghcr.io/paradigmxyz/op-reth:latest",
    "op-erigon": "testinprod/op-erigon:latest",
    "op-nethermind": "nethermindeth/nethermind:op-c482d56",
    "op-besu": "ghcr.io/optimism-java/op-besu:latest",
}

DEFAULT_CL_IMAGES = {
    "op-node": "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-node:develop",
    "hildr": "ghcr.io/optimism-java/hildr:latest",
}

DEFAULT_BATCHER_IMAGES = {
    "op-batcher": "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-batcher:develop",
}

DEFAULT_PROPOSER_IMAGES = {
    "op-proposer": "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-proposer:develop",
}

ATTR_TO_BE_SKIPPED_AT_ROOT = (
    "network_params",
    "participants",
)


DEFAULT_ADDITIONAL_SERVICES = []


def input_parser(plan, input_args):
    sanity_check.sanity_check(plan, input_args)
    result = parse_network_params(plan, input_args)

    return struct(
        participants=[
            struct(
                el_type=participant["el_type"],
                el_image=participant["el_image"],
                cl_type=participant["cl_type"],
                cl_image=participant["cl_image"],
                count=participant["count"],
            )
            for participant in result["participants"]
        ],
        network_params=struct(
            network=result["network_params"]["network"],
            network_id=result["network_params"]["network_id"],
            seconds_per_slot=result["network_params"]["seconds_per_slot"],
            name=result["network_params"]["name"],
            fjord_time_offset=result["network_params"]["fjord_time_offset"],
            granite_time_offset=result["network_params"]["granite_time_offset"],
            holocene_time_offset=result["network_params"]["holocene_time_offset"],
            interop_time_offset=result["network_params"]["interop_time_offset"],
        ),
        additional_services=result.get(
            "additional_services", DEFAULT_ADDITIONAL_SERVICES
        ),
        op_contract_deployer_params=struct(
            image=result["op_contract_deployer_params"]["image"],
        ),
    )


def parse_network_params(plan, input_args):
    result = default_input_args(input_args)

    for attr in input_args:
        value = input_args[attr]
        # if its insterted we use the value inserted
        if attr not in ATTR_TO_BE_SKIPPED_AT_ROOT and attr in input_args:
            result[attr] = value
        elif attr == "network_params":
            for sub_attr in input_args["network_params"]:
                sub_value = input_args["network_params"][sub_attr]
                result["network_params"][sub_attr] = sub_value
        elif attr == "participants":
            participants = []
            for participant in input_args["participants"]:
                new_participant = default_participant()
                for sub_attr, sub_value in participant.items():
                    # if the value is set in input we set it in participant
                    new_participant[sub_attr] = sub_value
                for _ in range(0, new_participant["count"]):
                    participant_copy = (
                        ethereum_package_input_parser.deep_copy_participant(
                            new_participant
                        )
                    )
                    participants.append(participant_copy)
            result["participants"] = participants

    for index, participant in enumerate(result["participants"]):
        el_type = participant["el_type"]
        cl_type = participant["cl_type"]
        el_image = participant["el_image"]
        if el_image == "":
            default_image = DEFAULT_EL_IMAGES.get(el_type, "")
            if default_image == "":
                fail(
                    "{0} received an empty image name and we don't have a default for it".format(
                        el_type
                    )
                )
            participant["el_image"] = default_image

        cl_image = participant["cl_image"]
        if cl_image == "":
            if cl_image == "":
                default_image = DEFAULT_CL_IMAGES.get(cl_type, "")
            if default_image == "":
                fail(
                    "{0} received an empty image name and we don't have a default for it".format(
                        cl_type
                    )
                )
            participant["cl_image"] = default_image

    return result


def default_input_args(input_args):
    network_params = default_network_params()
    participants = [default_participant()]
    op_contract_deployer_params = default_op_contract_deployer_params()
    return {
        "participants": participants,
        "network_params": network_params,
        "op_contract_deployer_params": op_contract_deployer_params,
    }


def default_network_params():
    return {
        "network": "kurtosis",
        "network_id": "2151908",
        "name": "op-kurtosis",
        "seconds_per_slot": 2,
        "fjord_time_offset": 0,
        "granite_time_offset": None,
        "holocene_time_offset": None,
        "interop_time_offset": None,
    }


def default_participant():
    return {
        "el_type": "op-geth",
        "el_image": "",
        "cl_type": "op-node",
        "cl_image": "",
        "count": 1,
    }


def default_op_contract_deployer_params():
    return {
        "image": "ethpandaops/optimism-contract-deployer:develop",
    }
