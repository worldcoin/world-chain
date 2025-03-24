constants = import_module("../../package_io/constants.star")
util = import_module("../../util.star")

ethereum_package_shared_utils = import_module(
    "github.com/ethpandaops/ethereum-package/src/shared_utils/shared_utils.star"
)

HTTP_PORT_NUMBER = 9080
GRPC_PORT_NUMBER = 0

TEMPLATES_FILEPATH = "./templates"

VALUES_FILE_NAME = "values.yaml"
VALUES_TEMPLATE_FILEPATH = "{0}/{1}.tmpl".format(TEMPLATES_FILEPATH, VALUES_FILE_NAME)

K8S_NAMESPACE_FILE = "/var/run/secrets/kubernetes.io/serviceaccount/namespace"


def launch_promtail(
    plan,
    global_node_selectors,
    loki_url,
    promtail_params,
):
    values_template = read_file(VALUES_TEMPLATE_FILEPATH)

    values_artifact_name = create_values_artifact(
        plan,
        values_template,
        global_node_selectors,
        loki_url,
    )

    install_helm_chart(
        plan,
        values_artifact_name,
        "promtail",
        "grafana",
        "https://grafana.github.io/helm-charts",
        override_name=True,
    )


def create_values_artifact(
    plan,
    values_template,
    node_selectors,
    loki_url,
):
    config_data = {
        "Ports": {
            "http": HTTP_PORT_NUMBER,
            "grpc": GRPC_PORT_NUMBER,
        },
        "LokiURL": loki_url,
        "NodeSelectors": node_selectors,
    }

    values_template_and_data = ethereum_package_shared_utils.new_template_and_data(
        values_template, config_data
    )

    values_artifact_name = plan.render_templates(
        {
            "/{0}".format(VALUES_FILE_NAME): values_template_and_data,
        },
        name="promtail-config",
    )

    return values_artifact_name


def install_helm_chart(
    plan,
    values_artifact_name,
    chart_name,
    repo_name=None,
    repo_url=None,
    override_name=False,
):
    cmds = []

    if repo_name != None and repo_url != None:
        cmds += [
            "helm repo add {0} {1}".format(repo_name, repo_url),
            "helm repo update",
        ]

    install_cmd = "helm upgrade --values /helm/{2} --install {1} {0}/{1}".format(
        repo_name, chart_name, VALUES_FILE_NAME
    )

    if override_name:
        install_cmd += " --set nameOverride=$(cat {0})".format(K8S_NAMESPACE_FILE)

    cmds.append(install_cmd)

    plan.run_sh(
        image="alpine/helm",
        files={
            "/helm": values_artifact_name,
        },
        run=util.join_cmds(cmds),
    )
