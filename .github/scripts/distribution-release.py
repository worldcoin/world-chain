"""Assemble unsigned release assets and finalize their external integrity manifest."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys
import tarfile
import zipfile


spec = importlib.util.spec_from_file_location("distribution_package", Path(__file__).with_name("distribution-package.py"))
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)
distribution = packaging.distribution
SHA256 = r"[0-9a-f]{64}"
MAX_RECORD_BYTES = 8 * 1024 * 1024


def reject_constant(value):
    raise ValueError(f"non-finite JSON value: {value}")


def load_record(path):
    with path.open("rb") as source:
        data = source.read(MAX_RECORD_BYTES + 1)
    return parse_record(data)


def parse_record(data):
    if len(data) > MAX_RECORD_BYTES:
        raise ValueError("release record exceeds 8 MiB")
    try:
        record = json.loads(data.decode("utf-8"), object_pairs_hook=distribution.unique_object,
                            parse_constant=reject_constant)
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise ValueError("release record must be bounded UTF-8 JSON") from error
    if not isinstance(record, dict):
        raise ValueError("release record must be a JSON object")
    return record


def digest(value, location, prefix=""):
    if not distribution.matches(value, re.escape(prefix) + SHA256):
        raise ValueError(f"{location} must be a lowercase {prefix}SHA-256 digest")


def validate_receipt(receipt):
    distribution.object_fields(receipt, (
        "schema_version", "source_date_epoch", "inputs", "packaging_toolchain", "payloads", "archives",
    ), "binary receipt")
    if type(receipt["schema_version"]) is not int or receipt["schema_version"] != 1:
        raise ValueError("binary receipt schema_version must be 1")
    epoch = receipt["source_date_epoch"]
    if type(epoch) is not int or not 0 <= epoch <= 0xFFFFFFFF:
        raise ValueError("binary receipt epoch must be an unsigned 32-bit timestamp")
    distribution.object_fields(receipt["packaging_toolchain"], ("python", "zlib"), "packaging toolchain")
    if any(not isinstance(value, str) or not value for value in receipt["packaging_toolchain"].values()):
        raise ValueError("packaging toolchain versions must be nonempty strings")
    plan = distribution.release_plan(receipt["inputs"])
    names = {name for platforms in plan["assets"]["binaries"].values() for name in platforms.values()}
    distribution.object_fields(receipt["archives"], sorted(names), "binary receipt archives")
    for name, value in receipt["archives"].items():
        digest(value, name)
    distribution.object_fields(receipt["payloads"], distribution.TARGETS, "binary receipt payloads")
    for platform, payload in receipt["payloads"].items():
        distribution.object_fields(payload, ("directory", "sha256"), "payload")
        if payload["directory"] != f"payloads/{platform.replace('/', '-')}":
            raise ValueError("unexpected binary receipt payload directory")
        hashes = payload["sha256"]
        required = {"README.md", "distribution-inputs.json", *(f"bin/{name}" for name in plan["binaries"])}
        if not isinstance(hashes, dict) or not required < set(hashes) or len(hashes) > packaging.MAX_FILES + 1:
            raise ValueError("payload must list the executables, metadata and nonempty licenses")
        for name, value in hashes.items():
            if name not in required and not (name.startswith("licenses/") and all(
                    re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}", part) for part in name.split("/"))
                    and len(name) <= 240):
                raise ValueError("unexpected payload file path")
            digest(value, name)
        if hashes["distribution-inputs.json"] != hashlib.sha256(packaging.json_bytes(receipt["inputs"])).hexdigest():
            raise ValueError("embedded distribution inputs do not match binary receipt inputs")
    return plan


def validate_records(inputs, plan, build_record, image_record):
    for name, record, field in (("build record", build_record, "materials"), ("image record", image_record, "images")):
        distribution.object_fields(record, ("schema_version", "inputs", field), name)
        distribution.release_plan(record["inputs"])
        if type(record["schema_version"]) is not int or record["schema_version"] != 1 or record["inputs"] != inputs:
            raise ValueError(f"{name} must use schema 1 and the exact assembly inputs")
    if not isinstance(build_record["materials"], dict) or not build_record["materials"]:
        raise ValueError("build materials must be a nonempty object")
    distribution.object_fields(image_record["images"], distribution.VARIANTS, "images")
    for variant, image in image_record["images"].items():
        distribution.object_fields(image, ("reference", "index_digest", "platforms"), f"images.{variant}")
        if image["reference"] != plan["images"][variant]["reference"]:
            raise ValueError("image reference must match the planned variant and version")
        digest(image["index_digest"], "image index", "sha256:")
        distribution.object_fields(image["platforms"], distribution.TARGETS, "image platforms")
        for platform in image["platforms"].values():
            distribution.object_fields(platform, ("manifest_digest", "config_digest"), "image platform")
            for name, value in platform.items():
                digest(value, name, "sha256:")
    if len({image["index_digest"] for image in image_record["images"].values()}) != len(distribution.VARIANTS):
        raise ValueError("distribution variants must have distinct image indexes")


def verify_archive(path, receipt, plan, platform, variant):
    hashes = receipt["payloads"][platform]["sha256"]
    expected = {name: value for name, value in hashes.items() if not name.startswith("bin/")}
    expected.update({name: hashes[f"bin/{name}"] for name in plan["images"][variant]["binaries"]})
    seen = set()
    with tarfile.open(path, "r|gz") as archive:
        for member in archive:
            name = member.name
            if name not in expected or name in seen or not member.isfile():
                raise ValueError(f"unexpected, repeated or non-file binary archive member: {name}")
            mode = 0o755 if name in plan["binaries"] else 0o644
            if ((member.mode, member.uid, member.gid, member.mtime) != (mode, 0, 0, receipt["source_date_epoch"])
                    or member.uname or member.gname or member.linkname or set(member.pax_headers) - {"path"}):
                raise ValueError(f"unexpected binary archive metadata: {name}")
            content = archive.extractfile(member)
            actual = hashlib.sha256()
            while chunk := content.read(packaging.CHUNK):
                actual.update(chunk)
            if actual.hexdigest() != expected[name]:
                raise ValueError(f"binary archive member checksum mismatch: {name}")
            seen.add(name)
    if seen != set(expected):
        raise ValueError("binary archive is missing payload files")


def verify_zip_inputs(path, inputs):
    with zipfile.ZipFile(path) as archive:
        matches = [info for info in archive.infolist() if info.filename == "release-metadata/sources.json"]
        if len(matches) != 1 or matches[0].file_size > MAX_RECORD_BYTES:
            raise ValueError("ZIP must contain one bounded release-metadata/sources.json")
        with archive.open(matches[0]) as source:
            metadata = parse_record(source.read(MAX_RECORD_BYTES + 1))
        if "inputs" not in metadata:
            raise ValueError("ZIP metadata must bind the exact assembly inputs")
        distribution.release_plan(metadata["inputs"])
        if metadata["inputs"] != inputs:
            raise ValueError("ZIP metadata must bind the exact assembly inputs")


def finalize(assembly, sources, examples, build_record, image_record, output):
    if any((directory / "INCOMPLETE").exists() for directory in (assembly, sources.parent, examples.parent)):
        raise ValueError("input assembly is incomplete")
    receipt = load_record(assembly / "binary-manifest.json")
    plan = validate_receipt(receipt)
    validate_records(receipt["inputs"], plan, build_record, image_record)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.mkdir(mode=0o700)
    incomplete = output / "INCOMPLETE"
    incomplete.write_text("Release finalization has not completed; do not sign or upload this directory.\n")
    assets = {}
    for variant, platforms in plan["assets"]["binaries"].items():
        for platform, name in platforms.items():
            value = packaging.snapshot_file(assembly / name, output / name, False, platform)
            if value != receipt["archives"][name]:
                raise ValueError(f"binary archive checksum mismatch: {name}")
            verify_archive(output / name, receipt, plan, platform, variant)
            assets[name] = {"sha256": value, "bytes": (output / name).stat().st_size}
    for kind, source in (("sources", sources), ("examples", examples)):
        name = plan["assets"][kind]
        value = packaging.snapshot_file(source, output / name, False, "")
        verify_zip_inputs(output / name, receipt["inputs"])
        assets[name] = {"sha256": value, "bytes": (output / name).stat().st_size}
    manifest = {
        "schema_version": 1, "inputs": receipt["inputs"], "source_date_epoch": receipt["source_date_epoch"],
        "packaging_toolchain": receipt["packaging_toolchain"], "payloads": receipt["payloads"],
        "assets": assets, "build_materials": build_record["materials"], "images": image_record["images"],
        "finalizer": {"python": sys.version.split()[0], "scripts": {
            name: packaging.file_digest(Path(__file__).with_name(name))
            for name in ("distribution-release.py", "distribution-package.py", "distribution-inputs.py")
        }},
    }
    packaging.atomic_write(output / plan["assets"]["manifest"], packaging.json_bytes(manifest))
    sums = {name: value["sha256"] for name, value in assets.items()}
    sums[plan["assets"]["manifest"]] = packaging.file_digest(output / plan["assets"]["manifest"])
    packaging.atomic_write(output / plan["assets"]["checksums"], "".join(
        f"{value}  {name}\n" for name, value in sorted(sums.items())).encode("ascii"))
    incomplete.unlink()
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("assembly", "sources", "examples", "build-record", "image-record", "output"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    args = parser.parse_args()
    try:
        finalize(args.assembly, args.sources, args.examples, load_record(args.build_record),
                 load_record(args.image_record), args.output)
        print(f"Assembled unsigned release assets in {args.output}; signing and release approval remain required")
    except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile, RuntimeError, EOFError) as error:
        print(f"release finalization failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
