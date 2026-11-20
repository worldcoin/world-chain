"""Verify staged distribution assets and public OCI bindings before release signing."""

import argparse
import hashlib
import importlib.util
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile


spec = importlib.util.spec_from_file_location("distribution_release", Path(__file__).with_name("distribution-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
REPOSITORY = "worldcoin/world-chain"
MAX_ASSET = 2 * 1024**3
MAX_TOTAL = 10 * 1024**3
INDEX_TYPES = {"application/vnd.oci.image.index.v1+json", "application/vnd.docker.distribution.manifest.list.v2+json"}
MANIFEST_TYPES = {"application/vnd.oci.image.manifest.v1+json", "application/vnd.docker.distribution.manifest.v2+json"}


def bounded_command(command, output, limit, timeout):
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    deadline, size, errors = time.monotonic() + timeout, 0, bytearray()
    completed = False
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            selector.register(process.stderr, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                ready = selector.select(min(30, max(0, remaining)))
                if remaining <= 0 or not ready:
                    raise TimeoutError(f"{command[0]} exceeded its request deadline or 30-second idle limit")
                for key, _ in ready:
                    chunk = os.read(key.fileobj.fileno(), 64 * 1024)
                    if not chunk:
                        selector.unregister(key.fileobj)
                    elif key.fileobj is process.stderr:
                        if len(errors) + len(chunk) > 64 * 1024:
                            raise ValueError(f"{command[0]} stderr exceeded its byte budget")
                        errors.extend(chunk)
                    else:
                        size += len(chunk)
                        if size > limit:
                            raise ValueError(f"{command[0]} response exceeded its byte budget")
                        output.write(chunk)
        code = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        completed = True
        if code:
            status = re.search(rb"HTTP (\d{3})", errors)
            detail = f"; HTTP {status[1].decode()}" if status else ""
            raise RuntimeError(f"{command[0]} failed with exit {code}{detail}")
    finally:
        process.stdout.close()
        process.stderr.close()
        if not completed:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass  # The group may already have exited during timeout cleanup.
            process.wait(timeout=5)


def command_record(command):
    with tempfile.TemporaryFile() as output:
        bounded_command(command, output, release.MAX_RECORD_BYTES, 120)
        output.seek(0)
        raw = output.read()
    return raw, release.parse_record(raw)


def github_release(identifier):
    if not re.fullmatch(r"[1-9][0-9]{0,19}", identifier):
        raise ValueError("release ID must be a positive decimal integer")
    return command_record(["gh", "api", "--hostname", "github.com", f"repos/{REPOSITORY}/releases/{identifier}"])[1]


def check_draft(record, identifier, version, source):
    release.distribution.commit(source, "release source")
    if not release.distribution.matches(version, release.distribution.BUNDLE_VERSION):
        raise ValueError("release version must be a canonical v-prefixed version")
    if (type(record.get("id")) is not int or str(record["id"]) != identifier
            or record.get("draft") is not True or record.get("published_at") is not None
            or record.get("tag_name") != version or record.get("target_commitish") != source):
        raise ValueError("release must remain a draft with the expected ID, version and full target commit")


def remote_draft(identifier, version, source):
    record = github_release(identifier)
    check_draft(record, identifier, version, source)
    prefix = ["gh", "api", "--hostname", "github.com"]
    refs = command_record([*prefix, f"repos/{REPOSITORY}/git/matching-refs/tags/{version}", "--jq", "{refs: .}"])[1].get("refs")
    if not isinstance(refs, list):
        raise ValueError("GitHub returned an invalid tag reference list")
    matches = [x for x in refs if isinstance(x, dict) and x.get("ref") == "refs/tags/" + version]
    if len(matches) > 1:
        raise ValueError("release tag reference is ambiguous")
    if matches:
        target = matches[0].get("object")
        for _ in range(8):
            if not isinstance(target, dict) or target.get("type") not in ("commit", "tag"):
                raise ValueError("release tag must resolve to a commit")
            release.distribution.commit(target.get("sha"), "tag object")
            if target["type"] == "commit":
                if target["sha"] != source:
                    raise ValueError("existing release tag points to a different source commit")
                break
            target = command_record([*prefix, f"repos/{REPOSITORY}/git/tags/{target['sha']}"])[1].get("object")
        else:
            raise ValueError("release tag chain exceeds eight objects")
    return record


def draft_assets(record):
    assets = record.get("assets")
    if not isinstance(assets, list) or len(assets) > 20:
        raise ValueError("draft has an invalid asset list")
    by_name = {}
    for asset in assets:
        if not isinstance(asset, dict) or not isinstance(asset.get("name"), str) or asset["name"] in by_name:
            raise ValueError("draft has invalid or duplicate asset metadata")
        if (type(asset.get("id")) is not int or asset["id"] <= 0 or asset.get("state") != "uploaded"
                or type(asset.get("size")) is not int or not 0 < asset["size"] <= MAX_ASSET):
            raise ValueError("draft has an invalid asset identity, state or size")
        by_name[asset["name"]] = asset
    return by_name


def download_asset(asset, output):
    try:
        bounded_command(["gh", "api", "--hostname", "github.com", "-H", "Accept: application/octet-stream",
                         f"repos/{REPOSITORY}/releases/assets/{asset['id']}"], output, asset["size"], 1200)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        raise RuntimeError(f"downloading draft asset {asset['id']} failed: {error}") from error


def validate_manifest(manifest, version, source):
    release.distribution.object_fields(manifest, (
        "schema_version", "inputs", "source_date_epoch", "packaging_toolchain", "payloads", "assets",
        "build_materials", "images", "finalizer",
    ), "release manifest")
    plan = release.distribution.release_plan(manifest["inputs"])
    if manifest["inputs"]["bundle_version"] != version or manifest["inputs"]["sources"]["world_chain"] != source:
        raise ValueError("manifest does not match the approved release version and source")
    names = {n for platforms in plan["assets"]["binaries"].values() for n in platforms.values()}
    release.distribution.object_fields(manifest["assets"], sorted(names | {plan["assets"]["sources"], plan["assets"]["examples"]}), "release assets")
    for name, record in manifest["assets"].items():
        release.distribution.object_fields(record, ("sha256", "bytes"), name)
        release.digest(record["sha256"], name)
        if type(record["bytes"]) is not int or not 0 < record["bytes"] <= MAX_ASSET:
            raise ValueError("release asset size must be positive and at most 2 GiB")
    if sum(r["bytes"] for r in manifest["assets"].values()) > MAX_TOTAL:
        raise ValueError("release assets exceed ten GiB")
    receipt = {k: manifest[k] for k in ("schema_version", "source_date_epoch", "inputs", "packaging_toolchain", "payloads")}
    receipt["archives"] = {n: manifest["assets"][n]["sha256"] for n in names}
    release.validate_receipt(receipt)
    release.validate_records(manifest["inputs"], plan,
                             {"schema_version": 1, "inputs": manifest["inputs"], "materials": manifest["build_materials"]},
                             {"schema_version": 1, "inputs": manifest["inputs"], "images": manifest["images"]})
    release.distribution.object_fields(manifest["finalizer"], ("python", "scripts"), "finalizer")
    if not isinstance(manifest["finalizer"]["python"], str) or not manifest["finalizer"]["python"]:
        raise ValueError("finalizer Python version must be a nonempty string")
    scripts = manifest["finalizer"]["scripts"]
    release.distribution.object_fields(scripts, ("distribution-release.py", "distribution-package.py", "distribution-inputs.py"), "finalizer scripts")
    for name, value in scripts.items():
        release.digest(value, name)
    return plan, receipt


def verified_manifest(directory, expected, version, source):
    release.digest(expected, "approved manifest")
    path = directory / "release-manifest.json"
    if (not stat.S_ISREG(path.lstat().st_mode) or path.stat().st_size > release.MAX_RECORD_BYTES
            or release.packaging.file_digest(path) != expected):
        raise ValueError("release manifest is not a regular file with the approved SHA-256")
    manifest = release.load_record(path)
    plan, receipt = validate_manifest(manifest, version, source)
    return manifest, plan, receipt


def verify_assets(directory, expected, version, source):
    manifest, plan, receipt = verified_manifest(directory, expected, version, source)
    sums = {name: row["sha256"] for name, row in manifest["assets"].items()}
    sums["release-manifest.json"] = expected
    wanted = "".join(f"{sha}  {name}\n" for name, sha in sorted(sums.items())).encode("ascii")
    checksums = directory / "SHA256SUMS"
    if (not stat.S_ISREG(checksums.lstat().st_mode) or checksums.stat().st_size > 4096
            or checksums.read_bytes() != wanted):
        raise ValueError("SHA256SUMS must exactly cover the eight assets and approved manifest")
    for name, row in manifest["assets"].items():
        path = directory / name
        if (not stat.S_ISREG(path.lstat().st_mode) or path.stat().st_size != row["bytes"]
                or release.packaging.file_digest(path) != row["sha256"]):
            raise ValueError(f"release asset size/type/checksum mismatch: {name}")
    for variant, platforms in plan["assets"]["binaries"].items():
        for platform, name in platforms.items():
            release.verify_archive(directory / name, receipt, plan, platform, variant)
    for kind in ("sources", "examples"):
        release.verify_zip_inputs(directory / plan["assets"][kind], manifest["inputs"])
    return manifest, plan


def verify_images(images):
    def inspect(reference, expected):
        raw, data = command_record(["docker", "buildx", "imagetools", "inspect", "--raw", reference])
        if "sha256:" + hashlib.sha256(raw).hexdigest() != expected:
            raise ValueError("published OCI bytes do not match the release digest")
        return data
    for image in images.values():
        index = inspect(image["reference"], image["index_digest"])
        if index.get("schemaVersion") != 2 or index.get("mediaType") not in INDEX_TYPES:
            raise ValueError("release image must resolve to an OCI index or Docker manifest list")
        manifests = index.get("manifests")
        if not isinstance(manifests, list) or len(manifests) > 100:
            raise ValueError("image index has an invalid descriptor list")
        repository = image["reference"].rsplit(":", 1)[0]
        for platform, pins in image["platforms"].items():
            os_name, arch = platform.split("/")
            matches = [x for x in manifests if isinstance(x, dict) and isinstance(x.get("platform"), dict) and x["platform"].get("os") == os_name
                       and x.get("platform", {}).get("architecture") == arch]
            if len(matches) != 1 or matches[0].get("digest") != pins["manifest_digest"]:
                raise ValueError("image platform descriptor does not match the release manifest")
            child = inspect(repository + "@" + pins["manifest_digest"], pins["manifest_digest"])
            if (child.get("schemaVersion") != 2 or child.get("mediaType") not in MANIFEST_TYPES
                    or not isinstance(child.get("config"), dict)
                    or child["config"].get("digest") != pins["config_digest"]):
                raise ValueError("image configuration descriptor does not match the release manifest")


def render_page(plan, source, changes):
    template = Path(__file__).parents[2] / "docs/release-page-preview.md"
    version = plan["inputs"]["bundle_version"]
    text = template.read_text().split("\n\n", 1)[1].replace("v2.8.3", version)
    base = f"https://github.com/{REPOSITORY}"
    downloads = f"{base}/releases/download/{version}/"
    docs = f"{base}/blob/{source}/"
    deployment = docs + "examples/distribution/README.md"
    build = docs + "docs/distribution-build.md"
    links = {"MANIFEST_URL": downloads + "release-manifest.json", "CHECKSUMS_URL": downloads + "SHA256SUMS",
             "SIGNATURES_URL": f"{base}/releases/tag/{version}",
             "SOURCES_ZIP_URL": downloads + plan["assets"]["sources"],
             "EXAMPLES_ZIP_URL": downloads + plan["assets"]["examples"],
             "DEPLOYMENT_GUIDE_URL": deployment, "BUILD_GUIDE_URL": build,
             "WORLD_CHAIN_IMAGE_GUIDE_URL": deployment + "#2-individual-images",
             "OP_IMAGE_GUIDE_URL": deployment + "#2-individual-images"}
    for name in ("WORLD_CHAIN", "OP", "COMBINED"):
        links[name + "_IMAGE_URL"] = base + "/pkgs/container/world-chain"
        links[name + "_BINARY_GUIDE_URL"] = deployment + "#3-direct-binaries-and-systemd"
        for arch, platform in (("AMD64", "linux/amd64"), ("ARM64", "linux/arm64")):
            links[name + "_" + arch + "_URL"] = downloads + plan["assets"]["binaries"][name.lower().replace("_", "-")][platform]
    for name in ("VERIFICATION", "SIGNATURE", "MANIFEST"):
        links[name + "_GUIDE_URL"] = build + "#finalize-the-unsigned-release-assets"
    for key, value in links.items():
        text = text.replace("{{" + key + "}}", value)
    if re.search(r"{{[A-Z_]+}}", text):
        raise ValueError("release-page template contains an unbound link")
    start, end = text.index("## Changes\n"), text.index("**Upgrade notes:**")
    return text[:start] + "## Changes\n\n" + changes.strip() + "\n\n" + text[end:]


def prepare(identifier, expected, version, source, output, changes, documentation_commit):
    release.distribution.commit(documentation_commit, "documentation source")
    by_name = draft_assets(remote_draft(identifier, version, source))
    output.mkdir(mode=0o700)
    marker = output / "INCOMPLETE"
    marker.write_text("Prepared release verification is incomplete; do not sign these assets.\n")
    def download(name, maximum):
        asset = by_name.get(name, {})
        if (type(asset.get("id")) is not int or asset["id"] <= 0 or asset.get("state") != "uploaded"
                or type(asset.get("size")) is not int or not 0 < asset["size"] <= maximum):
            raise ValueError(f"missing or invalid draft asset: {name}")
        with (output / name).open("xb") as destination:
            download_asset(asset, destination)
        if (output / name).stat().st_size != asset["size"]:
            raise ValueError(f"truncated draft asset: {name}")
    download("release-manifest.json", release.MAX_RECORD_BYTES)
    manifest, plan, _ = verified_manifest(output, expected, version, source)
    names = sorted([*manifest["assets"], "release-manifest.json", "SHA256SUMS"])
    if not set(names) <= set(by_name) <= set(names) | {n + ".asc" for n in names}:
        raise ValueError("draft must contain exactly the planned assets and optional detached signatures")
    for name in names:
        if name != "release-manifest.json":
            limit = manifest["assets"][name]["bytes"] if name in manifest["assets"] else 4096
            download(name, limit)
    verify_assets(output, expected, version, source)
    verify_images(manifest["images"])
    remote_draft(identifier, version, source)
    (output / "release-files.txt").write_text("".join(n + "\n" for n in names))
    (output / "release-notes.md").write_text(render_page(plan, documentation_commit, changes))
    marker.unlink()


def verify_uploaded(identifier, expected, version, source, directory):
    manifest, _, _ = verified_manifest(directory, expected, version, source)
    unsigned = sorted([*manifest["assets"], "release-manifest.json", "SHA256SUMS"])
    by_name = draft_assets(remote_draft(identifier, version, source))
    if set(by_name) != set(unsigned) | {n + ".asc" for n in unsigned}:
        raise ValueError("signed draft must contain all ten verified files and their detached signatures")
    for name, asset in by_name.items():
        path = directory / name
        if not stat.S_ISREG(path.lstat().st_mode) or path.stat().st_size != asset["size"]:
            raise ValueError("uploaded asset type/size differs from the verified local file")
        expected_sha = release.packaging.file_digest(path)
        if asset.get("digest") is not None:
            if asset["digest"] != "sha256:" + expected_sha:
                raise ValueError("uploaded asset digest differs from the verified local file")
        with tempfile.TemporaryFile() as downloaded:
            download_asset(asset, downloaded)
            downloaded.seek(0)
            actual = hashlib.sha256()
            while chunk := downloaded.read(release.packaging.CHUNK):
                actual.update(chunk)
            if actual.hexdigest() != expected_sha:
                raise ValueError("uploaded asset bytes differ from the verified local file")
    verify_images(manifest["images"])
    remote_draft(identifier, version, source)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("release-id", "manifest-sha256", "version", "source-commit", "output"):
        parser.add_argument("--" + name, required=True, type=Path if name == "output" else str)
    parser.add_argument("--changes", type=Path)
    parser.add_argument("--documentation-commit")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check-draft", action="store_true")
    mode.add_argument("--verify-uploaded", action="store_true")
    args = parser.parse_args()
    try:
        release.digest(args.manifest_sha256, "approved manifest")
        if args.check_draft:
            remote_draft(args.release_id, args.version, args.source_commit)
        elif args.verify_uploaded:
            verify_uploaded(args.release_id, args.manifest_sha256, args.version, args.source_commit, args.output)
        else:
            if args.changes is None or not 0 < args.changes.stat().st_size <= 64 * 1024:
                raise ValueError("a nonempty changes file of at most 64 KiB is required")
            prepare(args.release_id, args.manifest_sha256, args.version, args.source_commit,
                    args.output, args.changes.read_text(), args.documentation_commit)
        print("Prepared release checks passed; no release, tag or image was published")
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile, EOFError) as error:
        print(f"prepared release verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
