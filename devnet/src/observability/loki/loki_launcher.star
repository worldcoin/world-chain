constants = import_module("../../package_io/constants.star")
util = import_module("../../util.star")

ethereum_package_shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

SERVICE_NAME = "loki"
HTTP_PORT_NUMBER = 3100
GRPC_PORT_NUMBER = 9096

TEMPLATES_FILEPATH = "./templates"

CONFIG_FILE_NAME = "loki-config.yaml"
CONFIG_TEMPLATE_FILEPATH = "{0}/{1}.tmpl".format(TEMPLATES_FILEPATH, CONFIG_FILE_NAME)

CONFIG_DIRPATH_ON_SERVICE = "/config"


USED_PORTS = {
    constants.HTTP_PORT_ID: ethereum_package_shared_utils.new_port_spec(
        HTTP_PORT_NUMBER,
        ethereum_package_shared_utils.TCP_PROTOCOL,
        ethereum_package_shared_utils.HTTP_APPLICATION_PROTOCOL,
    ),
    "grpc": ethereum_package_shared_utils.new_port_spec(
        GRPC_PORT_NUMBER,
        ethereum_package_shared_utils.TCP_PROTOCOL,
        "grpc",
    ),
}


def launch_loki(
    plan,
    global_node_selectors,
    loki_params,
):
    config_template = read_file(CONFIG_TEMPLATE_FILEPATH)

    config_artifact_name = create_config_artifact(
        plan,
        config_template,
    )

    service_config = get_service_config(
        config_artifact_name,
        global_node_selectors,
        loki_params,
    )

    service = plan.add_service(SERVICE_NAME, service_config)

    service_url = util.make_service_http_url(service)

    return service_url


def create_config_artifact(
    plan,
    config_template,
):
    config_data = {
        "Ports": {
            "http": HTTP_PORT_NUMBER,
            "grpc": GRPC_PORT_NUMBER,
        },
    }
    config_template_and_data = ethereum_package_shared_utils.new_template_and_data(
        config_template, config_data
    )

    template_and_data_by_rel_dest_filepath = {
        CONFIG_FILE_NAME: config_template_and_data,
    }

    config_artifact_name = plan.render_templates(
        template_and_data_by_rel_dest_filepath, name="loki-config"
    )

    return config_artifact_name


def get_service_config(
    config_artifact_name,
    node_selectors,
    loki_params,
):
    return ServiceConfig(
        image=loki_params.image,
        ports=USED_PORTS,
        cmd=[
            "-config.file={0}/{1}".format(CONFIG_DIRPATH_ON_SERVICE, CONFIG_FILE_NAME),
        ],
        files={
            CONFIG_DIRPATH_ON_SERVICE: config_artifact_name,
        },
        min_cpu=loki_params.min_cpu,
        max_cpu=loki_params.max_cpu,
        min_memory=loki_params.min_mem,
        max_memory=loki_params.max_mem,
        node_selectors=node_selectors,
    )
