"""Snapshot prebuilt payloads and assemble deterministic binary bundle variants."""

import argparse
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import struct
import sys
import tarfile
import zlib


spec = importlib.util.spec_from_file_location("distribution_inputs", Path(__file__).with_name("distribution-inputs.py"))
distribution = importlib.util.module_from_spec(spec)
spec.loader.exec_module(distribution)

MACHINES = {"linux/amd64": 62, "linux/arm64": 183}
CHUNK = 1024 * 1024
MAX_FILES = 10000


def json_bytes(value):
    return (json.dumps(value, sort_keys=True, indent=2, allow_nan=False) + "\n").encode("utf-8")


def real_directory(path):
    if path.is_symlink() or not path.is_dir():
        raise ValueError(f"expected a real directory: {path}")


def payload_files(root, binaries):
    real_directory(root)
    if {p.name for p in root.iterdir()} != {"bin", "licenses", "README.md"}:
        raise ValueError("each payload must contain exactly bin/, licenses/ and README.md")
    real_directory(root / "bin")
    real_directory(root / "licenses")
    if {p.name for p in (root / "bin").iterdir()} != set(binaries):
        raise ValueError("payload bin/ must contain exactly the planned executables")
    files = [Path("README.md"), *(Path("bin") / name for name in binaries)]

    def raise_walk_error(error):
        raise error

    for parent, directories, names in os.walk(root / "licenses", onerror=raise_walk_error, followlinks=False):
        for name in directories:
            real_directory(Path(parent) / name)
        for name in names:
            relative = (Path(parent) / name).relative_to(root)
            if len(relative.as_posix()) > 240 or any(
                re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}", part) is None
                for part in relative.parts
            ):
                raise ValueError("license paths must use bounded ASCII file names")
            files.append(relative)
            if len(files) > MAX_FILES:
                raise ValueError("payload contains too many files")
    if len(files) == len(binaries) + 1:
        raise ValueError("payload licenses/ must not be empty")
    return sorted(files, key=lambda p: p.as_posix())


def check_elf(header, platform):
    if (len(header) < 64 or header[:7] != b"\x7fELF\x02\x01\x01"
            or header[7] not in (0, 3)
            or struct.unpack_from("<H", header, 16)[0] not in (2, 3)
            or struct.unpack_from("<H", header, 18)[0] != MACHINES[platform]
            or struct.unpack_from("<I", header, 20)[0] != 1
            or struct.unpack_from("<H", header, 52)[0] != 64):
        raise ValueError(f"binary must have a 64-bit little-endian ELF header for {platform}")


def snapshot_file(source, destination, executable, platform):
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as reader:
        before = os.fstat(reader.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size == 0:
            raise ValueError(f"payload file must be a nonempty regular file: {source}")
        header = reader.read(64)
        if executable:
            check_elf(header, platform)
        destination.parent.mkdir(parents=True, exist_ok=True)
        digest = hashlib.sha256()
        with destination.open("xb") as writer:
            digest.update(header)
            writer.write(header)
            while True:
                chunk = reader.read(CHUNK)
                if not chunk:
                    break
                digest.update(chunk)
                writer.write(chunk)
        after = os.fstat(reader.fileno())
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ValueError(f"payload changed while being read: {source}")
    destination.chmod(0o755 if executable else 0o644)
    return digest.hexdigest()


def file_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            chunk = source.read(CHUNK)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def write_archive(path, snapshot, members, epoch):
    partial = path.with_name(path.name + ".part")
    with partial.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch, compresslevel=6) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as archive:
                for name, relative in sorted(members.items()):
                    source = snapshot / relative
                    info = tarfile.TarInfo(name)
                    info.size = source.stat().st_size
                    info.mode = 0o755 if relative.startswith("bin/") else 0o644
                    info.mtime = epoch
                    with source.open("rb") as content:
                        archive.addfile(info, content)
    partial.rename(path)
    return file_digest(path)


def atomic_write(path, data):
    temporary = path.with_name(path.name + ".part")
    with temporary.open("xb") as target:
        target.write(data)
    temporary.rename(path)


def package(inputs, payload_root, output, epoch):
    plan = distribution.release_plan(inputs)
    if type(epoch) is not int or not 0 <= epoch <= 0xFFFFFFFF:
        raise ValueError("source-date-epoch must be an unsigned 32-bit Unix timestamp")
    payload_root = payload_root.resolve(strict=True)
    if output.resolve().is_relative_to(payload_root):
        raise ValueError("output must be outside the input payload tree")
    inventories = {
        platform: payload_files(payload_root / platform.replace("/", "-"), plan["binaries"])
        for platform in distribution.TARGETS
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.mkdir(mode=0o700)
    incomplete = output / "INCOMPLETE"
    incomplete.write_text("Binary assembly has not completed; do not publish this directory.\n")
    receipt = {
        "schema_version": 1, "source_date_epoch": epoch, "inputs": inputs,
        "packaging_toolchain": {"python": sys.version.split()[0], "zlib": zlib.ZLIB_RUNTIME_VERSION},
        "payloads": {}, "archives": {},
    }
    for platform, files in inventories.items():
        slug = platform.replace("/", "-")
        snapshot = output / "payloads" / slug
        hashes = {}
        for relative in files:
            executable = relative.parts[0] == "bin"
            hashes[relative.as_posix()] = snapshot_file(
                payload_root / slug / relative, snapshot / relative, executable, platform)
        metadata = snapshot / "distribution-inputs.json"
        metadata.write_bytes(json_bytes(inputs))
        metadata.chmod(0o644)
        hashes[metadata.name] = file_digest(metadata)
        receipt["payloads"][platform] = {"directory": f"payloads/{slug}", "sha256": hashes}
    for platform in distribution.TARGETS:
        snapshot = output / "payloads" / platform.replace("/", "-")
        common = {
            relative.as_posix(): relative.as_posix()
            for relative in inventories[platform] if relative.parts[0] != "bin"
        }
        common["distribution-inputs.json"] = "distribution-inputs.json"
        for variant, image in plan["images"].items():
            # Flatten executable paths to preserve the existing archive-root layout.
            members = {**common, **{name: f"bin/{name}" for name in image["binaries"]}}
            filename = plan["assets"]["binaries"][variant][platform]
            receipt["archives"][filename] = write_archive(output / filename, snapshot, members, epoch)
    checksums = "".join(f"{digest}  {name}\n" for name, digest in sorted(receipt["archives"].items()))
    atomic_write(output / "SHA256SUMS", checksums.encode("ascii"))
    atomic_write(output / "binary-manifest.json", json_bytes(receipt))
    incomplete.unlink()
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", required=True, type=Path)
    parser.add_argument("--payload-root", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--source-date-epoch", required=True, type=int)
    args = parser.parse_args()
    try:
        receipt = package(distribution.load_inputs(args.inputs), args.payload_root, args.output, args.source_date_epoch)
        print(f"Assembled {len(receipt['archives'])} binary archives in {args.output}")
    except (OSError, ValueError, tarfile.TarError) as error:
        print(f"binary assembly failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
