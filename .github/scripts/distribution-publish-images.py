"""Copy pinned, qualified OCI images from staging to GHCR for an RC."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys


PLATFORMS = ("linux/amd64", "linux/arm64")
VARIANTS = {"world-chain": "", "op": "op-", "combined": "combined-"}
SHA256 = re.compile(r"sha256:[0-9a-f]{64}\Z")
RC_VERSION = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+-rc\.[1-9][0-9]*\Z")
INDEX_TYPES = {"application/vnd.oci.image.index.v1+json", "application/vnd.docker.distribution.manifest.list.v2+json"}
MANIFEST_TYPES = {"application/vnd.oci.image.manifest.v1+json", "application/vnd.docker.distribution.manifest.v2+json"}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON object field")
        result[key] = value
    return result


def reject_constant(value):
    raise ValueError(f"invalid JSON constant: {value}")


def command(*args, timeout=300):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout).stdout


def inspect(reference):
    raw = command("docker", "buildx", "imagetools", "inspect", "--raw", reference)
    try:
        record = json.loads(raw, object_pairs_hook=unique_object, parse_constant=reject_constant)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise ValueError(f"invalid OCI JSON for {reference}") from error
    if not isinstance(record, dict):
        raise ValueError(f"invalid OCI object for {reference}")
    return hashlib.sha256(raw).hexdigest(), record


def validate_inputs(version, source, digests):
    if not RC_VERSION.fullmatch(version):
        raise ValueError("image publication requires a canonical RC version")
    if not re.fullmatch(r"[0-9a-f]{40}", source) or source == "0" * 40:
        raise ValueError("image publication requires a full immutable source commit")
    if not isinstance(digests, dict) or set(digests) != set(VARIANTS):
        raise ValueError("image digest map must contain the three distribution variants")
    values = []
    for variant, platforms in digests.items():
        if not isinstance(platforms, dict) or set(platforms) != set(PLATFORMS):
            raise ValueError(f"{variant} must contain both target platforms")
        for platform in PLATFORMS:
            value = platforms[platform]
            if not isinstance(value, str) or not SHA256.fullmatch(value):
                raise ValueError(f"{variant} {platform} must use a SHA-256 digest")
            values.append(value)
    if len(set(values)) != len(values):
        raise ValueError("image digests must be distinct")


def platform_manifest(registry, digest, platform):
    actual, index = inspect(f"{registry}@{digest}")
    if digest != f"sha256:{actual}" or index.get("mediaType") not in INDEX_TYPES:
        raise ValueError(f"staged {platform} image index does not match its digest")
    descriptors = index.get("manifests")
    if not isinstance(descriptors, list):
        raise ValueError("staged image index has no descriptors")
    runnable = [item for item in descriptors if isinstance(item, dict)
                and isinstance(item.get("platform"), dict) and item["platform"].get("os") == "linux"]
    if len(runnable) != 1:
        raise ValueError("staged image must contain exactly one runnable platform")
    matches = [item for item in descriptors if isinstance(item, dict)
               and isinstance(item.get("platform"), dict)
               and item["platform"].get("os") == "linux"
               and item["platform"].get("architecture") == platform.split("/", 1)[1]]
    if len(matches) != 1 or matches[0].get("mediaType") not in MANIFEST_TYPES:
        raise ValueError(f"staged image has no unique {platform} manifest")
    child = matches[0].get("digest")
    if not isinstance(child, str) or not SHA256.fullmatch(child):
        raise ValueError("staged image has an invalid platform manifest digest")
    actual, manifest = inspect(f"{registry}@{child}")
    if child != f"sha256:{actual}" or manifest.get("mediaType") not in MANIFEST_TYPES:
        raise ValueError("staged platform manifest does not match its digest")
    config_record = manifest.get("config")
    config = config_record.get("digest") if isinstance(config_record, dict) else None
    if not isinstance(config, str) or not SHA256.fullmatch(config):
        raise ValueError("staged platform manifest has an invalid configuration digest")
    return {"manifest_digest": child, "config_digest": config}


def attestation_manifest(registry, digest, platform_digest):
    actual, index = inspect(f"{registry}@{digest}")
    if digest != f"sha256:{actual}" or index.get("mediaType") not in INDEX_TYPES:
        raise ValueError("staged attestation index does not match its digest")
    matches = [item for item in index.get("manifests", []) if isinstance(item, dict)
               and isinstance(item.get("annotations"), dict)
               and item["annotations"].get("vnd.docker.reference.type") == "attestation-manifest"
               and item["annotations"].get("vnd.docker.reference.digest") == platform_digest]
    if (len(matches) != 1 or not isinstance(matches[0].get("digest"), str)
            or not SHA256.fullmatch(matches[0]["digest"])):
        raise ValueError("staged image must contain one bound attestation manifest")
    attestation = matches[0]["digest"]
    actual, _ = inspect(f"{registry}@{attestation}")
    if attestation != f"sha256:{actual}":
        raise ValueError("staged attestation manifest does not match its digest")
    return attestation


def verify_published(reference, expected, attestations=None):
    index_hash, index = inspect(reference)
    if index.get("mediaType") not in INDEX_TYPES or not isinstance(index.get("manifests"), list):
        raise ValueError("published image is not a multi-platform index")
    runnable = [item for item in index["manifests"] if isinstance(item, dict)
                and isinstance(item.get("platform"), dict) and item["platform"].get("os") == "linux"]
    if len(runnable) != len(PLATFORMS):
        raise ValueError("published image must contain exactly two runnable platforms")
    if attestations is not None:
        descriptors = [item for item in index["manifests"] if isinstance(item, dict)
                       and isinstance(item.get("annotations"), dict)
                       and item["annotations"].get("vnd.docker.reference.type") == "attestation-manifest"]
        if (len(descriptors) != 2 or any(not isinstance(item.get("digest"), str) for item in descriptors)
                or {item["digest"] for item in descriptors} != set(attestations.values())):
            raise ValueError("published attestations changed from staging")
        for platform, digest in attestations.items():
            matching = [item for item in descriptors if item["digest"] == digest]
            if (len(matching) != 1 or matching[0]["annotations"].get("vnd.docker.reference.digest")
                    != expected[platform]["manifest_digest"]):
                raise ValueError(f"published {platform} attestation is unbound")
            attestation_hash, _ = inspect(f"{reference.rsplit(':', 1)[0]}@{digest}")
            if f"sha256:{attestation_hash}" != digest:
                raise ValueError(f"published {platform} attestation manifest changed")
    for platform, pins in expected.items():
        descriptors = [item for item in index["manifests"] if isinstance(item, dict)
                       and isinstance(item.get("platform"), dict)
                       and item["platform"].get("os") == "linux"
                       and item["platform"].get("architecture") == platform.split("/", 1)[1]]
        if len(descriptors) != 1 or descriptors[0].get("digest") != pins["manifest_digest"]:
            raise ValueError(f"published {platform} image changed from the staged manifest")
        child_hash, child = inspect(f"{reference.rsplit(':', 1)[0]}@{pins['manifest_digest']}")
        config_record = child.get("config")
        config_digest = config_record.get("digest") if isinstance(config_record, dict) else None
        if (f"sha256:{child_hash}" != pins["manifest_digest"]
                or config_digest != pins["config_digest"]):
            raise ValueError(f"published {platform} configuration changed from staging")
    return f"sha256:{index_hash}"


def publish(version, source, digests, staging, destination, output):
    validate_inputs(version, source, digests)
    images = {}
    attestations = {}
    for variant, prefix in VARIANTS.items():
        pins = {platform: platform_manifest(staging, digests[variant][platform], platform)
                for platform in PLATFORMS}
        staged_attestations = {platform: attestation_manifest(staging, digests[variant][platform],
                                                               pins[platform]["manifest_digest"])
                               for platform in PLATFORMS}
        reference = f"{destination}:{prefix}{version}"
        try:
            verify_published(reference, pins, staged_attestations)
        except subprocess.CalledProcessError as error:
            message = error.stderr.decode("utf-8", "replace").lower()
            if "not found" not in message and "manifest unknown" not in message:
                raise
            command("docker", "buildx", "imagetools", "create", "--tag", reference,
                    *(f"{staging}@{digests[variant][platform]}" for platform in PLATFORMS), timeout=1200)
        index_digest = verify_published(reference, pins, staged_attestations)
        images[variant] = {"reference": reference, "index_digest": index_digest, "platforms": pins}
        attestations[variant] = staged_attestations
        print(f"Verified {reference}@{index_digest}", flush=True)
    output.write_text(json.dumps({"source_commit": source, "version": version,
                                  "staging_digests": digests, "images": images,
                                  "attestation_manifests": attestations},
                                 sort_keys=True, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--digests", required=True)
    parser.add_argument("--staging", required=True)
    parser.add_argument("--destination", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        if len(args.digests.encode()) > 4096:
            raise ValueError("image digest map exceeds 4 KiB")
        digests = json.loads(args.digests, object_pairs_hook=unique_object, parse_constant=reject_constant)
        publish(args.version, args.source_commit, digests, args.staging, args.destination, args.output)
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or b"").decode("utf-8", "replace").strip()[-1000:]
        print(f"image publication command failed: {detail or error}", file=sys.stderr)
        return 1
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"image publication failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
