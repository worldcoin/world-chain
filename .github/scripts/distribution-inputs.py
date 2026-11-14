"""Validate offline distribution inputs and emit the shared release asset plan."""

import argparse
import json
from pathlib import Path
import re
import sys


COMPONENTS = (
    "op-node", "op-batcher", "op-proposer", "op-conductor", "op-challenger",
    "op-deployer", "op-dispute-mon",
    "cannon", "kona-host",
)
TARGETS = {
    "linux/amd64": "x86_64-unknown-linux-gnu",
    "linux/arm64": "aarch64-unknown-linux-gnu",
}
NUMBER = r"(?:0|[1-9][0-9]*)"
BASE_VERSION = rf"v{NUMBER}\.{NUMBER}\.{NUMBER}"
BUNDLE_VERSION = rf"{BASE_VERSION}(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?"
WORLDCHAIN_VERSION = rf"({BASE_VERSION})-worldchain(?:\.[1-9][0-9]*)?"
COMMIT = r"[0-9a-f]{40}"
MAX_INPUT_BYTES = 64 * 1024


def object_fields(value, fields, location):
    if not isinstance(value, dict) or set(value) != set(fields):
        raise ValueError(f"{location} must contain exactly: {', '.join(fields)}")


def matches(value, pattern):
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def commit(value, location):
    if not matches(value, COMMIT) or value == "0" * 40:
        raise ValueError(f"{location} must be a nonzero lowercase 40-character commit ID")


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON object field")
        result[key] = value
    return result


def load_inputs(path):
    with path.open("rb") as source:
        data = source.read(MAX_INPUT_BYTES + 1)
    if len(data) > MAX_INPUT_BYTES:
        raise ValueError("distribution inputs exceed 64 KiB")
    try:
        return json.loads(data.decode("utf-8"), object_pairs_hook=unique_object)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise ValueError("distribution inputs must be valid UTF-8 JSON") from error
    except RecursionError as error:
        raise ValueError("distribution input JSON nesting is too deep") from error


def release_plan(inputs):
    object_fields(inputs, ("schema_version", "bundle_version", "sources", "components", "platforms"), "inputs")
    if type(inputs["schema_version"]) is not int or inputs["schema_version"] != 1:
        raise ValueError("schema_version must be 1")
    version = inputs["bundle_version"]
    if not matches(version, BUNDLE_VERSION) or len(version) > 128:
        raise ValueError("bundle_version must be a canonical World Chain release version starting with vX.Y.Z")
    sources = inputs["sources"]
    object_fields(sources, ("world_chain", "optimism"), "sources")
    for repository, value in sources.items():
        commit(value, f"sources.{repository}")
    components = inputs["components"]
    object_fields(components, COMPONENTS, "components")
    for name in COMPONENTS:
        component = components[name]
        object_fields(component, ("version", "upstream_tag", "upstream_commit"), f"components.{name}")
        version_value = component["version"]
        match = re.fullmatch(WORLDCHAIN_VERSION, version_value) if isinstance(version_value, str) else None
        if match is None or len(version_value) > 128:
            raise ValueError(f"components.{name}.version must be vX.Y.Z-worldchain with an optional positive .N revision")
        if component["upstream_tag"] != f"{name}/{match[1]}":
            raise ValueError(f"components.{name}.upstream_tag must match its component and base version")
        commit(component["upstream_commit"], f"components.{name}.upstream_commit")
    platforms = inputs["platforms"]
    if (not isinstance(platforms, list) or len(platforms) != len(TARGETS)
            or any(not isinstance(p, str) for p in platforms)
            or set(platforms) != set(TARGETS)):
        raise ValueError("platforms must contain linux/amd64 and linux/arm64 exactly once")

    return {
        "inputs": inputs,
        "binaries": ["world-chain", *COMPONENTS, "forge", "cast"],
        "image_binary_directory": "/usr/local/bin",
        "assets": {
            "binaries": {
                platform: f"world-chain-{version}-{target}.tar.gz"
                for platform, target in TARGETS.items()
            },
            "sources": f"world-chain-{version}-sources.tar.gz",
            "manifest": "release-manifest.json",
            "checksums": "SHA256SUMS",
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", type=Path, help="pinned distribution input JSON")
    args = parser.parse_args()
    try:
        plan = release_plan(load_inputs(args.inputs))
        json.dump(plan, sys.stdout, indent=2, sort_keys=True, allow_nan=False)
        sys.stdout.write("\n")
    except (OSError, ValueError) as error:
        print(f"distribution inputs: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
