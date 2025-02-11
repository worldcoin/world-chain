ethereum_package_shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

postgres = import_module("github.com/kurtosis-tech/postgres-package/main.star")

util = import_module("../util.star")

IMAGE_NAME_BLOCKSCOUT = "blockscout/blockscout-optimism:6.8.0"
IMAGE_NAME_BLOCKSCOUT_VERIF = "ghcr.io/blockscout/smart-contract-verifier:v1.9.0"

SERVICE_NAME_BLOCKSCOUT = "op-blockscout"

HTTP_PORT_ID = "http"
HTTP_PORT_NUMBER = 4000
HTTP_PORT_NUMBER_VERIF = 8050

BLOCKSCOUT_MIN_CPU = 100
BLOCKSCOUT_MAX_CPU = 1000
BLOCKSCOUT_MIN_MEMORY = 1024
BLOCKSCOUT_MAX_MEMORY = 2048

BLOCKSCOUT_VERIF_MIN_CPU = 10
BLOCKSCOUT_VERIF_MAX_CPU = 1000
BLOCKSCOUT_VERIF_MIN_MEMORY = 10
BLOCKSCOUT_VERIF_MAX_MEMORY = 1024

USED_PORTS = {
    HTTP_PORT_ID: ethereum_package_shared_utils.new_port_spec(
        HTTP_PORT_NUMBER,
        ethereum_package_shared_utils.TCP_PROTOCOL,
        ethereum_package_shared_utils.HTTP_APPLICATION_PROTOCOL,
    )
}

VERIF_USED_PORTS = {
    HTTP_PORT_ID: ethereum_package_shared_utils.new_port_spec(
        HTTP_PORT_NUMBER_VERIF,
        ethereum_package_shared_utils.TCP_PROTOCOL,
        ethereum_package_shared_utils.HTTP_APPLICATION_PROTOCOL,
    )
}


def launch_blockscout(
    plan,
    l2_services_suffix,
    l1_rpc_url,
    l2_el_context,
    l2_network_name,
    deployment_output,
    network_id,
):
    rollup_filename = "rollup-{0}".format(network_id)
    portal_address = util.read_network_config_value(
        plan, deployment_output, rollup_filename, ".deposit_contract_address"
    )
    l1_deposit_start_block = util.read_network_config_value(
        plan, deployment_output, rollup_filename, ".genesis.l1.number"
    )

    postgres_output = postgres.run(
        plan,
        service_name="{0}-postgres{1}".format(
            SERVICE_NAME_BLOCKSCOUT, l2_services_suffix
        ),
        database="blockscout",
        extra_configs=["max_connections=1000"],
    )

    config_verif = get_config_verif()
    verif_service_name = "{0}-verif{1}".format(
        SERVICE_NAME_BLOCKSCOUT, l2_services_suffix
    )
    verif_service = plan.add_service(verif_service_name, config_verif)
    verif_url = "http://{}:{}".format(
        verif_service.hostname, verif_service.ports["http"].number
    )

    config_backend = get_config_backend(
        postgres_output,
        l1_rpc_url,
        l2_el_context,
        verif_url,
        l2_network_name,
        {
            "INDEXER_OPTIMISM_L1_PORTAL_CONTRACT": portal_address,
            "INDEXER_OPTIMISM_L1_DEPOSITS_START_BLOCK": l1_deposit_start_block,
            "INDEXER_OPTIMISM_L1_WITHDRAWALS_START_BLOCK": l1_deposit_start_block,
            "INDEXER_OPTIMISM_L1_BATCH_START_BLOCK": l1_deposit_start_block,
            # The L2OO is no longer deployed
            "INDEXER_OPTIMISM_L1_OUTPUT_ORACLE_CONTRACT": "0x0000000000000000000000000000000000000000",
        },
    )
    blockscout_service = plan.add_service(
        "{0}{1}".format(SERVICE_NAME_BLOCKSCOUT, l2_services_suffix), config_backend
    )
    plan.print(blockscout_service)

    blockscout_url = "http://{}:{}".format(
        blockscout_service.hostname, blockscout_service.ports["http"].number
    )

    return blockscout_url


def get_config_verif():
    return ServiceConfig(
        image=IMAGE_NAME_BLOCKSCOUT_VERIF,
        ports=VERIF_USED_PORTS,
        env_vars={
            "SMART_CONTRACT_VERIFIER__SERVER__HTTP__ADDR": "0.0.0.0:{}".format(
                HTTP_PORT_NUMBER_VERIF
            )
        },
        min_cpu=BLOCKSCOUT_VERIF_MIN_CPU,
        max_cpu=BLOCKSCOUT_VERIF_MAX_CPU,
        min_memory=BLOCKSCOUT_VERIF_MIN_MEMORY,
        max_memory=BLOCKSCOUT_VERIF_MAX_MEMORY,
    )


def get_config_backend(
    postgres_output,
    l1_rpc_url,
    l2_el_context,
    verif_url,
    l2_network_name,
    additional_env_vars,
):
    database_url = "{protocol}://{user}:{password}@{hostname}:{port}/{database}".format(
        protocol="postgresql",
        user=postgres_output.user,
        password=postgres_output.password,
        hostname=postgres_output.service.hostname,
        port=postgres_output.port.number,
        database=postgres_output.database,
    )

    optimism_env_vars = {
        "CHAIN_TYPE": "optimism",
        "INDEXER_OPTIMISM_L1_RPC": l1_rpc_url,
        # "INDEXER_OPTIMISM_L1_PORTAL_CONTRACT": "",
        # "INDEXER_OPTIMISM_L1_BATCH_START_BLOCK": "",
        "INDEXER_OPTIMISM_L1_BATCH_INBOX": "0xff00000000000000000000000000000000042069",
        "INDEXER_OPTIMISM_L1_BATCH_SUBMITTER": "0x776463f498A63a42Ac1AFc7c64a4e5A9ccBB4d32",
        "INDEXER_OPTIMISM_L1_BATCH_BLOCKSCOUT_BLOBS_API_URL": verif_url + "/blobs",
        "INDEXER_OPTIMISM_L1_BATCH_BLOCKS_CHUNK_SIZE": "4",
        "INDEXER_OPTIMISM_L2_BATCH_GENESIS_BLOCK_NUMBER": "0",
        "INDEXER_OPTIMISM_L1_OUTPUT_ROOTS_START_BLOCK": "0",
        # "INDEXER_OPTIMISM_L1_OUTPUT_ORACLE_CONTRACT": l2oo_address,
        # "INDEXER_OPTIMISM_L1_DEPOSITS_START_BLOCK": l1_deposit_start_block,
        "INDEXER_OPTIMISM_L1_DEPOSITS_BATCH_SIZE": "500",
        # "INDEXER_OPTIMISM_L1_WITHDRAWALS_START_BLOCK": l1_deposit_start_block,
        "INDEXER_OPTIMISM_L2_WITHDRAWALS_START_BLOCK": "1",
        "INDEXER_OPTIMISM_L2_MESSAGE_PASSER_CONTRACT": "0xC0D3C0d3C0d3c0d3C0d3C0D3c0D3c0d3c0D30016",
    } | additional_env_vars

    return ServiceConfig(
        image=IMAGE_NAME_BLOCKSCOUT,
        ports=USED_PORTS,
        cmd=[
            "/bin/sh",
            "-c",
            'bin/blockscout eval "Elixir.Explorer.ReleaseTasks.create_and_migrate()" && bin/blockscout start',
        ],
        env_vars={
            "ETHEREUM_JSONRPC_VARIANT": "geth",
            "ETHEREUM_JSONRPC_HTTP_URL": l2_el_context.rpc_http_url,
            "ETHEREUM_JSONRPC_TRACE_URL": l2_el_context.rpc_http_url,
            "DATABASE_URL": database_url,
            "COIN": "opETH",
            "MICROSERVICE_SC_VERIFIER_ENABLED": "true",
            "MICROSERVICE_SC_VERIFIER_URL": verif_url,
            "MICROSERVICE_SC_VERIFIER_TYPE": "sc_verifier",
            "INDEXER_DISABLE_PENDING_TRANSACTIONS_FETCHER": "true",
            "ECTO_USE_SSL": "false",
            "NETWORK": l2_network_name,
            "SUBNETWORK": l2_network_name,
            "API_V2_ENABLED": "true",
            "PORT": "{}".format(HTTP_PORT_NUMBER),
            "SECRET_KEY_BASE": "56NtB48ear7+wMSf0IQuWDAAazhpb31qyc7GiyspBP2vh7t5zlCsF5QDv76chXeN",
        }
        | optimism_env_vars,
        min_cpu=BLOCKSCOUT_MIN_CPU,
        max_cpu=BLOCKSCOUT_MAX_CPU,
        min_memory=BLOCKSCOUT_MIN_MEMORY,
        max_memory=BLOCKSCOUT_MAX_MEMORY,
    )
