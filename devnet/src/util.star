DEPLOYMENT_UTILS_IMAGE = "mslipper/deployment-utils:latest"


def read_network_config_value(plan, network_config_file, json_file, json_path):
    mounts = {"/network-data": network_config_file}
    return read_json_value(
        plan, "/network-data/{0}.json".format(json_file), json_path, mounts
    )


def read_json_value(plan, json_file, json_path, mounts=None):
    run = plan.run_sh(
        description="Read JSON value",
        image=DEPLOYMENT_UTILS_IMAGE,
        files=mounts,
        run="jq -j '{1}' < {0}".format(json_file, json_path),
    )
    return run.output


def read_file(plan, file_path, mounts=None):
    run = plan.run_sh(
        description="Read file",
        image=DEPLOYMENT_UTILS_IMAGE,
        files=mounts,
        run="cat {0}".format(file_path),
    )
    return run.output


def write_to_file(plan, contents, directory, file_name):
    file_path = "{0}/{1}".format(directory, file_name)

    run = plan.run_sh(
        description="Write value to a file artifact",
        image=DEPLOYMENT_UTILS_IMAGE,
        store=[file_path],
        run="mkdir -p '{0}' && echo '{2}' > '{1}'".format(
            directory, file_path, contents
        ),
    )

    return run.files_artifacts[0]


def to_hex_chain_id(chain_id):
    out = "%x" % int(chain_id)
    pad = 64 - len(out)
    return "0x" + "0" * pad + out


def label_from_image(image):
    """Generate a label from an image name.

    Truncate the image label to 63 characters max to comply with k8s label length limit.
    The label is expected to be in the format of <registry>/<image>:<tag>.
    But it case it's too long, we take the longest meaningful suffix
    (so that it's still recognizable), breaking at slashes (so that it's valid).

    Args:
        image: The image to label.

    Returns:
        The potentially truncated label.
    """
    max_length = 63
    if len(image) <= max_length:
        return image
    cpts = image.split("/")
    label = cpts[-1]
    for cpt in reversed(cpts[:-1]):
        if len(label) + len(cpt) + 1 > max_length:
            break
        label = cpt + "/" + label
    # just in case the last part is already too long
    if len(label) > max_length:
        label = label[-max_length:]
    return label


def join_cmds(commands):
    return " && ".join(commands)
