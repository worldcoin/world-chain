"""Export pinned Git sources, submodules and deployment examples without changing checkouts."""

import argparse
from collections import deque
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import stat
import subprocess
import sys
import tempfile
from urllib.parse import unquote, urlsplit
import zipfile
import zlib


spec = importlib.util.spec_from_file_location("distribution_release", Path(__file__).with_name("distribution-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
packaging = release.packaging
distribution = release.distribution
MAX_FILES = 100000
MAX_BYTES = 10 * 1024**3
MAX_BLOB = 512 * 1024**2


def safe_path(value):
    path = PurePosixPath(value)
    if (not value or not path.parts or path.is_absolute() or path.as_posix() != value
            or any(part in (".", "..") or part.casefold() == ".git" for part in path.parts)
            or "\\" in value or any(ord(c) < 32 for c in value) or len(value.encode()) > 4096):
        raise ValueError("source paths must be canonical relative paths without Git metadata")
    return path


def git(repo, *args, input=None, stdout=subprocess.PIPE):
    env = dict(os.environ, GIT_NO_LAZY_FETCH="1", GIT_NO_REPLACE_OBJECTS="1", GIT_TERMINAL_PROMPT="0")
    result = subprocess.run(["git", "-C", str(repo), *args], input=input, stdout=stdout,
                            stderr=subprocess.PIPE, env=env, timeout=120)
    if result.returncode:
        raise ValueError(f"Git {args[0]} failed with exit {result.returncode}; check the local object store")
    return result.stdout


def tree_entries(repo, revision):
    entries = []
    for row in git(repo, "ls-tree", "-r", "-z", revision).split(b"\0"):
        if not row:
            continue
        header, raw_path = row.split(b"\t", 1)
        mode, kind, oid = header.decode("ascii").split()
        name = raw_path.decode("utf-8")
        safe_path(name)
        distribution.commit(oid, "Git object")
        if (mode, kind) not in (("100644", "blob"), ("100755", "blob"), ("120000", "blob"), ("160000", "commit")):
            raise ValueError("unsupported source entry type")
        entries.append((name, mode, oid))
        if len(entries) > MAX_FILES:
            raise ValueError("source tree contains too many entries")
    return entries


def source_nodes(inputs, world_chain, optimism, repositories):
    distribution.release_plan(inputs)
    providers = []

    def register(path, required=False):
        path = path.resolve()
        if not (path / ".git").exists():
            if required:
                raise ValueError("source repositories must be existing working checkouts")
            return
        if path not in providers:
            if Path(git(path, "rev-parse", "--show-toplevel").decode().strip()).resolve() != path:
                raise ValueError("source provider is not a repository root")
            providers.append(path)
            if len(providers) > 256:
                raise ValueError("too many local source repositories")

    def contains(repo, revision):
        result = git(repo, "cat-file", "--batch-check=%(objectname) %(objecttype)", input=(revision + "\n").encode()).strip()
        if result == (revision + " missing").encode():
            return False
        if result != (revision + " commit").encode():
            raise ValueError("source revision must identify a commit")
        return True

    for repo in (world_chain, optimism, *repositories):
        register(repo, required=True)
    for repo in list(providers):
        for name, mode, _ in tree_entries(repo, "HEAD"):
            if mode == "160000":
                register(repo / name)
    roots = {"world-chain": (world_chain.resolve(), inputs["sources"]["world_chain"]),
             "optimism": (optimism.resolve(), inputs["sources"]["optimism"])}
    if inputs["schema_version"] == 2:
        for name, component in sorted(inputs["components"].items()):
            if component["build_commit"] != inputs["sources"]["optimism"]:
                roots[f"component-sources/{name}"] = (optimism.resolve(), component["build_commit"])
    nodes = []

    def walk(prefix, preferred, revision, depth):
        safe_path(prefix)
        if depth > 16 or len(nodes) >= 2000:
            raise ValueError("source submodule graph exceeds its bounds")
        choices = [preferred, *(p for p in providers if p != preferred)]
        repo = next((p for p in choices if p in providers and contains(p, revision)), None)
        if repo is None:
            raise ValueError(f"missing local commit for {prefix}: {revision}; fetch it before exporting")
        entries = tree_entries(repo, revision)
        tree = git(repo, "rev-parse", revision + "^{tree}").decode().strip()
        distribution.commit(tree, "source tree")
        node = {"prefix": prefix, "repository": repo, "commit": revision, "tree": tree, "entries": entries}
        nodes.append(node)
        for name, mode, oid in entries:
            if mode == "160000":
                register(repo / name)
                walk(prefix + "/" + name, (repo / name).resolve(), oid, depth + 1)

    for prefix, (repo, revision) in roots.items():
        walk(prefix, repo, revision, 0)
    return nodes


def object_sizes(node):
    files = [(name, mode, oid) for name, mode, oid in node["entries"] if mode != "160000"]
    data = git(node["repository"], "cat-file", "--batch-check", input="".join(oid + "\n" for _, _, oid in files).encode())
    rows = data.splitlines()
    if len(rows) != len(files):
        raise ValueError("incomplete Git object inventory")
    sizes = []
    for (_, _, oid), row in zip(files, rows):
        parts = row.decode("ascii").split()
        if len(parts) != 3 or parts[:2] != [oid, "blob"] or not parts[2].isdigit():
            raise ValueError("missing or invalid source blob")
        size = int(parts[2])
        if size > MAX_BLOB:
            raise ValueError("source blob exceeds 512 MiB")
        sizes.append(size)
    return files, sizes


def blobs(node, files, sizes, scratch):
    with tempfile.TemporaryFile(dir=scratch) as batch:
        git(node["repository"], "cat-file", "--batch", input="".join(oid + "\n" for _, _, oid in files).encode(), stdout=batch)
        expected = sum(len(f"{oid} blob {size}\n".encode()) + size + 1 for (_, _, oid), size in zip(files, sizes))
        if batch.tell() != expected:
            raise ValueError("Git source changed after object inventory")
        batch.seek(0)
        for (name, mode, oid), size in zip(files, sizes):
            if batch.readline() != f"{oid} blob {size}\n".encode():
                raise ValueError("unexpected source object header")
            data = batch.read(size)
            if batch.read(1) != b"\n" or hashlib.sha1(f"blob {size}\0".encode() + data).hexdigest() != oid:
                raise ValueError("source blob content does not match its Git identity")
            if data.startswith(b"version https://git-lfs.github.com/spec/v1\n"):
                raise ValueError("source contains an unresolved Git LFS pointer")
            yield name, mode, oid, data


def zip_entry(archive, name, data, mode, timestamp):
    safe_path(name)
    if mode == "120000":
        target = data.decode("utf-8")
        if not target or target.startswith("/") or "\\" in target or "\0" in target:
            raise ValueError("unsafe source symlink")
        safe_path(posixpath.normpath(str(PurePosixPath(name).parent / target)))
    info = zipfile.ZipInfo(name, timestamp)
    info.create_system, info.compress_type = 3, zipfile.ZIP_DEFLATED
    # Git tree modes omit symlink permissions; ZIP extractors restore them.
    permissions = 0o777 if mode == "120000" else 0
    info.external_attr = (int(mode, 8) | permissions) << 16
    archive.writestr(info, data, compresslevel=6)


def validate_symlinks(archive):
    links = {info.filename: archive.read(info).decode("utf-8") for info in archive.infolist()
             if stat.S_ISLNK(info.external_attr >> 16)}
    for name in links:
        pending, resolved, followed = deque(name.split("/")), [], 0
        while pending:
            part = pending.popleft()
            if part in ("", "."):
                continue
            if part == "..":
                if not resolved:
                    raise ValueError("source symlink escapes the archive")
                resolved.pop()
                continue
            resolved.append(part)
            target = links.get("/".join(resolved))
            if target is not None:
                followed += 1
                if followed > 40:
                    raise ValueError("source symlink chain exceeds 40 links or contains a cycle")
                resolved.pop()
                pending.extendleft(reversed(target.split("/")))


def guide_paths(world_chain, files):
    entries = {name: (mode, oid) for name, mode, oid in files}
    pending = [name for name in entries if name.startswith("examples/distribution/") and name.endswith(".md")]
    result = set()
    while pending:
        path = pending.pop()
        if path in result:
            continue
        mode, oid = entries[path]
        if mode == "120000":
            raise ValueError("deployment guides must be regular Markdown files")
        text = git(world_chain, "cat-file", "blob", oid).decode("utf-8")
        result.add(path)
        inline = re.findall(r'''\[[^\]]*\]\(\s*(?:<([^<>\n]+)>|([^\s()]+))(?:\s+["'][^\n]*["'])?\s*\)''', text)
        references = re.findall(r"(?m)^ {0,3}\[[^\]]+\]:\s*(?:<([^<>\n]+)>|(\S+))", text)
        for angle, plain in inline + references:
            target = angle or plain
            url = urlsplit(target)
            if url.scheme or url.netloc or not url.path or "{{" in target:
                continue
            target_path = unquote(url.path)
            if not target_path.lower().endswith(".md"):
                continue
            name = posixpath.normpath(str(PurePosixPath(path).parent / target_path))
            safe_path(name)
            if name not in entries:
                raise ValueError(f"missing linked deployment guide: {name}")
            pending.append(name)
    return result


def package(inputs, world_chain, optimism, repositories, build_record, output, *, private_review=False,
            documentation_commit=None):
    plan = distribution.release_plan(inputs)
    distribution.object_fields(build_record, ("schema_version", "inputs", "materials"), "build record")
    if (type(build_record["schema_version"]) is not int or build_record["schema_version"] != 1
            or packaging.json_bytes(build_record["inputs"]) != packaging.json_bytes(inputs)
            or not isinstance(build_record["materials"], dict) or not build_record["materials"]):
        raise ValueError("build record must bind the exact inputs and reviewed materials")
    nodes = source_nodes(inputs, world_chain, optimism, repositories)
    inventories = [object_sizes(node) for node in nodes]
    if sum(len(files) for files, _ in inventories) > MAX_FILES or sum(sum(sizes) for _, sizes in inventories) > MAX_BYTES:
        raise ValueError("source export exceeds 100000 files or ten GiB")
    roots = {"world-chain", "optimism"}
    roots.update(f"component-sources/{name}" for name in inputs["components"])
    timestamps = {
        n["prefix"]: int(git(n["repository"], "show", "--no-show-signature", "-s", "--format=%ct", n["commit"]).strip())
        for n in nodes if n["prefix"] in roots
    }
    wc = next(node for node in nodes if node["prefix"] == "world-chain")
    epoch = timestamps["world-chain"]
    if not 0 <= epoch <= 0xFFFFFFFF:
        raise ValueError("source epoch must be an unsigned 32-bit timestamp")
    files, _ = next(inventory for node, inventory in zip(nodes, inventories) if node is wc)
    guides = guide_paths(wc["repository"], files)
    example_node, example_guides = wc, guides
    if documentation_commit is not None:
        distribution.commit(documentation_commit, "documentation source")
        repo = wc["repository"]
        if git(repo, "cat-file", "-t", documentation_commit).strip() != b"commit":
            raise ValueError("documentation source must be a commit")
        git(repo, "merge-base", "--is-ancestor", wc["commit"], documentation_commit)
        example_node = {**wc, "entries": tree_entries(repo, documentation_commit)}
        example_entries, _ = object_sizes(example_node)
        example_guides = guide_paths(repo, example_entries)
    example_node = {**example_node, "entries": [
        entry for entry in example_node["entries"]
        if entry[0].startswith("examples/distribution/") or entry[0] in example_guides
    ]}
    example_files, example_sizes = object_sizes(example_node)
    if not example_files or sum(example_sizes) > MAX_BYTES:
        raise ValueError("deployment examples are missing or exceed the source export size limit")
    when = datetime.fromtimestamp(max(epoch, 315532800), timezone.utc)
    timestamp = (when.year, when.month, when.day, when.hour, when.minute, when.second // 2 * 2)
    metadata = {"schema_version": 1, "inputs": inputs, "source_date_epoch": epoch,
                "build_materials": build_record["materials"], "guides": sorted(guides),
                "toolchain": {"python": sys.version.split()[0], "zlib": zlib.ZLIB_RUNTIME_VERSION},
                "sources": [{"prefix": n["prefix"], "commit": n["commit"], "tree": n["tree"],
                             **({"commit_timestamp": timestamps[n["prefix"]]} if n["prefix"] in timestamps else {}),
                             "gitlinks": [{"path": name, "commit": oid} for name, mode, oid in n["entries"] if mode == "160000"]}
                            for n in nodes]}
    if private_review:
        metadata["export_selection"] = {"requires_publication_review": True}
    output.parent.mkdir(parents=True, exist_ok=True)
    output.mkdir(mode=0o700)
    marker = output / "INCOMPLETE"
    marker.write_text("Source export is incomplete; do not sign or upload this directory.\n")
    indexes = {"sources": {}, "examples": {}}
    with zipfile.ZipFile(output / (plan["assets"]["sources"] + ".part"), "x") as sources, \
            zipfile.ZipFile(output / (plan["assets"]["examples"] + ".part"), "x") as examples:
        for node, (files, sizes) in zip(nodes, inventories):
            for path, mode, oid, data in blobs(node, files, sizes, output):
                name = node["prefix"] + "/" + path
                if name in indexes["sources"]:
                    raise ValueError("overlapping source trees")
                record = {"git_blob": oid, "mode": mode, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
                zip_entry(sources, name, data, mode, timestamp)
                indexes["sources"][name] = record
        for path, mode, oid, data in blobs(example_node, example_files, example_sizes, output):
            zip_entry(examples, path, data, mode, timestamp)
            indexes["examples"][path] = {
                "git_blob": oid, "mode": mode, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()
            }
        example_metadata = metadata
        if documentation_commit is not None:
            example_metadata = {**metadata, "documentation_commit": documentation_commit,
                                "guides": sorted(example_guides)}
        for kind, archive in (("sources", sources), ("examples", examples)):
            validate_symlinks(archive)
            zip_metadata = example_metadata if kind == "examples" else metadata
            zip_entry(archive, "release-metadata/sources.json", packaging.json_bytes(zip_metadata), "100644", timestamp)
            zip_entry(archive, "release-metadata/files.json", packaging.json_bytes(indexes[kind]), "100644", timestamp)
    receipt = {"schema_version": 1, "inputs": inputs, "source_date_epoch": epoch, "assets": {}}
    for kind in ("sources", "examples"):
        name = plan["assets"][kind]
        (output / (name + ".part")).rename(output / name)
        receipt["assets"][name] = {"sha256": packaging.file_digest(output / name), "bytes": (output / name).stat().st_size}
    packaging.atomic_write(output / "source-manifest.json", packaging.json_bytes(receipt))
    marker.unlink()
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("inputs", "world-chain-repo", "optimism-repo", "build-record", "output"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    parser.add_argument("--repository", action="append", type=Path, default=[], help="additional checkout containing pinned submodule objects")
    parser.add_argument("--private-review", action="store_true", help="mark both ZIPs as ineligible for release finalization")
    parser.add_argument("--documentation-commit", help="full descendant commit for the examples ZIP only")
    args = parser.parse_args()
    try:
        package(distribution.load_inputs(args.inputs), args.world_chain_repo, args.optimism_repo,
                args.repository, release.load_record(args.build_record), args.output, private_review=args.private_review,
                documentation_commit=args.documentation_commit)
        print(f"Exported source/examples assets to {args.output}; independent qualification remains required")
    except (OSError, ValueError, subprocess.SubprocessError, UnicodeError) as error:
        print(f"source export failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
