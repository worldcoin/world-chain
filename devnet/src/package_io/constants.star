EL_TYPE = struct(
    op_geth="op-geth",
    op_erigon="op-erigon",
    op_nethermind="op-nethermind",
    op_besu="op-besu",
    op_reth="op-reth",
)

CL_TYPE = struct(
    op_node="op-node",
    hildr="hildr",
)

CLIENT_TYPES = struct(
    el="execution",
    cl="beacon",
)
VOLUME_SIZE = {
    "kurtosis": {
        "op_geth_volume_size": 5000,  # 5GB
        "op_erigon_volume_size": 3000,  # 3GB
        "op_nethermind_volume_size": 3000,  # 3GB
        "op_besu_volume_size": 3000,  # 3GB
        "op_reth_volume_size": 3000,  # 3GB
        "op_node_volume_size": 1000,  # 1GB
        "hildr_volume_size": 1000,  # 1GB
    },
}

MEMPOOL_CONFIG_MOUNT = "/mempool_config"
MEMPOOL_CONFIG_MOUNT_PATH = MEMPOOL_CONFIG_MOUNT + "/mempool_config.json"
ENTRYPOINT_CONFIG_MOUNT = "/entrypoint_config"
ENTRYPOINT_CONFIG_MOUNT_PATH = ENTRYPOINT_CONFIG_MOUNT + "/entrypoint_config.json"
CHAIN_SPEC_MOUNT = "/chain_spec"
CHAIN_SPEC_MOUNT_PATH = CHAIN_SPEC_MOUNT + "/chain_spec.json"
