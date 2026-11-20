import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from test_distribution_inputs import component_source_inputs
from test_distribution_package import packaging
from test_distribution_release import records


SCRIPT = Path(__file__).with_name("distribution-sources.py")
spec = importlib.util.spec_from_file_location("distribution_sources", SCRIPT)
sources = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sources)


def oid(data):
    return hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()


class SourceExportTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.inputs = component_source_inputs()
        self.build, _ = records(self.inputs)
        self.data = {
            "README.md": b"Committed project source.\n",
            "examples/distribution/README.md": b'[guide](../../docs/setup.md "Setup")\n[more][more]\n[more]: ../../docs/extra.md\n',
            "examples/distribution/start.sh": b"#!/bin/sh\nexit 0\n",
            "docs/setup.md": b"[nested](<nested.md>)\n",
            "docs/nested.md": b"[back](setup.md)\n",
            "docs/extra.md": b"Extra committed guide.\n",
            "link": b"README.md",
        }
        entries = [(name, "120000" if name == "link" else "100755" if name.endswith(".sh") else "100644", oid(data))
                   for name, data in sorted(self.data.items())]
        self.nodes = [{"prefix": prefix, "repository": self.root / prefix,
                       "commit": self.inputs["sources"][field], "tree": "a" * 40, "entries": entries}
                      for prefix, field in (("world-chain", "world_chain"), ("optimism", "optimism"))]
        self.output = self.root / "export"

    def fake_git(self, repo, *args, input=None, stdout=subprocess.PIPE):
        if args[0] == "show":
            if repo == self.root / "optimism":
                return b"1700000002\n"
            return b"1700000001\n"
        if args[:2] == ("cat-file", "blob"):
            return next(data for data in self.data.values() if oid(data) == args[2])
        if args[:2] == ("cat-file", "--batch-check"):
            return b"".join(key + b" blob " + str(len(next(data for data in self.data.values() if oid(data).encode() == key))).encode() + b"\n"
                            for key in input.splitlines())
        if args[:2] == ("cat-file", "--batch"):
            for key in input.splitlines():
                data = next(data for data in self.data.values() if oid(data).encode() == key)
                stdout.write(key + b" blob " + str(len(data)).encode() + b"\n" + data + b"\n")
            return None
        self.fail(f"Unexpected Git fixture command: {args}")

    def package(self):
        with patch.object(sources, "source_nodes", return_value=self.nodes), patch.object(sources, "git", side_effect=self.fake_git):
            return sources.package(self.inputs, self.root, self.root, [], self.build, self.output)

    def test_export_preserves_blobs_modes_inputs_guides_and_reproducibility(self):
        (self.root / "untracked.env").write_text("Synthetic untracked sentinel")
        receipt = self.package()
        self.assertFalse((self.output / "INCOMPLETE").exists())
        self.assertEqual(len(receipt["assets"]), 2)
        self.assertEqual(json.loads((self.output / "source-manifest.json").read_text()), receipt)
        for name, record in receipt["assets"].items():
            self.assertEqual(packaging.file_digest(self.output / name), record["sha256"])
            sources.release.verify_zip_inputs(self.output / name, self.inputs)
            with zipfile.ZipFile(self.output / name) as archive:
                metadata = json.loads(archive.read("release-metadata/sources.json"))
                self.assertNotIn("repository", metadata["sources"][0])
                self.assertEqual({node["prefix"]: node["commit_timestamp"] for node in metadata["sources"]},
                                 {"world-chain": 1700000001, "optimism": 1700000002})
                self.assertEqual(metadata["source_date_epoch"], 1700000001)
                self.assertEqual(metadata["build_materials"], self.build["materials"])
                index = json.loads(archive.read("release-metadata/files.json"))
                self.assertEqual(set(index) | {"release-metadata/sources.json", "release-metadata/files.json"}, set(archive.namelist()))
                for relative, expected in index.items():
                    data = archive.read(relative)
                    self.assertEqual(oid(data), expected["git_blob"])
                    self.assertEqual(hashlib.sha256(data).hexdigest(), expected["sha256"])
                    mode = 0o120777 if expected["mode"] == "120000" else int(expected["mode"], 8)
                    self.assertEqual(archive.getinfo(relative).external_attr >> 16, mode)
                if "-examples.zip" in name:
                    self.assertEqual(set(index), {p for p in self.data if p.startswith("examples/") or p.startswith("docs/")})
                else:
                    self.assertEqual(len(index), 2 * len(self.data))
        first = self.output
        self.output = self.root / "repeated"
        self.assertEqual(receipt, self.package())
        for path in first.iterdir():
            self.assertEqual(path.read_bytes(), (self.output / path.name).read_bytes())

    def test_invalid_materials_and_existing_output_fail_without_overwriting(self):
        original = self.build
        self.build = copy.deepcopy(original)
        self.build["inputs"]["sources"]["world_chain"] = "9" * 40
        with self.assertRaises(ValueError):
            self.package()
        self.assertFalse(self.output.exists())
        self.build = original
        self.output.mkdir()
        sentinel = self.output / "keep"
        sentinel.write_text("existing export")
        with self.assertRaises(FileExistsError):
            self.package()
        self.assertEqual(sentinel.read_text(), "existing export")

    def test_export_ignores_configured_commit_signature_display(self):
        real_git = sources.git
        def git(repo, *args, **kwargs):
            if args[0] == "show":
                return real_git(SCRIPT.resolve().parents[2], *args[:-1], "HEAD", **kwargs)
            return self.fake_git(repo, *args, **kwargs)
        settings = {"GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "log.showSignature", "GIT_CONFIG_VALUE_0": "true"}
        with patch.dict(sources.os.environ, settings), patch.object(sources, "source_nodes", return_value=self.nodes), \
                patch.object(sources, "git", side_effect=git):
            receipt = sources.package(self.inputs, self.root, self.root, [], self.build, self.output)
        self.assertGreater(receipt["source_date_epoch"], 0)

    def test_bad_symlinks_and_git_metadata_paths_fail(self):
        for path in ("../outside", "/absolute", "x/.git/config", "x/.GiT/config", "a\\b", "a/../b"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                sources.safe_path(path)
        for target in (b"../../escape", b"/absolute", b"a\0b", b"a\\b"):
            with self.subTest(target=target), zipfile.ZipFile(self.root / "bad.zip", "w") as archive:
                with self.assertRaises(ValueError):
                    sources.zip_entry(archive, "world-chain/link", target, "120000", (2026, 1, 1, 0, 0, 0))

    def test_export_rejects_chained_symlink_escape_and_cycles(self):
        entries, data = list(self.nodes[0]["entries"]), dict(self.data)
        for links in ({"sub/up": b"..", "sub/escape": b"up/../../outside"},
                      {"a": b"b", "b": b"a"},
                      {"examples/distribution/sub/up": b"..",
                       "examples/distribution/sub/escape": b"up/../../../outside"}):
            with self.subTest(links=links), tempfile.TemporaryDirectory(dir=self.root) as output:
                self.output = Path(output) / "export"
                self.data = {**data, **links}
                self.nodes[0]["entries"] = entries + [(name, "120000", oid(target)) for name, target in links.items()]
                with self.assertRaisesRegex(ValueError, "symlink"):
                    self.package()
                self.assertTrue((self.output / "INCOMPLETE").exists())
                self.assertFalse((self.output / "source-manifest.json").exists())

    def test_export_preserves_safe_chained_parent_symlinks(self):
        links = {"sub/readme": b"up/README.md", "sub/up": b".."}
        self.data.update(links)
        self.nodes[0]["entries"].extend((name, "120000", oid(data)) for name, data in links.items())
        receipt = self.package()
        name = next(name for name in receipt["assets"] if name.endswith("-sources.zip"))
        with zipfile.ZipFile(self.output / name) as archive:
            for path, target in links.items():
                self.assertEqual(archive.read("world-chain/" + path), target)

    def test_object_reader_rejects_corruption_and_lfs_pointers(self):
        for data in (b"corrupt", b"version https://git-lfs.github.com/spec/v1\n"):
            key = oid(data) if data.startswith(b"version ") else "b" * 40
            def write(*args, **kwargs):
                kwargs["stdout"].write(f"{key} blob {len(data)}\n".encode() + data + b"\n")
            with self.subTest(data=data), patch.object(sources, "git", side_effect=write), self.assertRaises(ValueError):
                list(sources.blobs(self.nodes[0], [("README.md", "100644", key)], [len(data)], self.root))

    def test_failed_export_keeps_private_incomplete_output(self):
        with patch.object(sources, "blobs", side_effect=ValueError("object read failed")), self.assertRaises(ValueError):
            self.package()
        self.assertTrue((self.output / "INCOMPLETE").exists())
        self.assertFalse((self.output / "source-manifest.json").exists())

    def test_source_graph_requires_component_and_submodule_commits(self):
        wc, op, module = self.root / "wc", self.root / "op", self.root / "wc/deps/module"
        for repo in (wc, op, module):
            (repo / ".git").mkdir(parents=True)
        child = "d" * 40
        wc_commit, op_commit = self.inputs["sources"].values()
        extra = {c["build_commit"] for c in self.inputs["components"].values()} - {op_commit}
        available = {wc: {wc_commit}, op: {op_commit, *extra}, module: {child}}
        def git(repo, *args, input=None, **kwargs):
            if args == ("rev-parse", "--show-toplevel"):
                return (str(repo) + "\n").encode()
            if args[0] == "rev-parse":
                return ("a" * 40 + "\n").encode()
            revision = input.decode().strip()
            return (revision + (" commit\n" if revision in available[repo] else " missing\n")).encode()
        def entries(repo, revision):
            return [("deps/module", "160000", child)] if repo == wc else []
        with patch.object(sources, "git", side_effect=git), patch.object(sources, "tree_entries", side_effect=entries):
            nodes = sources.source_nodes(self.inputs, wc, op, [])
            expected = {"world-chain", "optimism", "world-chain/deps/module"}
            expected.update("component-sources/" + name for name, c in self.inputs["components"].items()
                            if c["build_commit"] != op_commit)
            self.assertEqual({n["prefix"] for n in nodes}, expected)
            reordered = copy.deepcopy(self.inputs)
            reordered["components"] = dict(reversed(list(reordered["components"].items())))
            self.assertEqual(nodes, sources.source_nodes(reordered, wc, op, []))
            available[module] = set()
            with self.assertRaisesRegex(ValueError, "missing local commit"):
                sources.source_nodes(self.inputs, wc, op, [])

    def test_real_git_blob_reader_and_missing_objects(self):
        repo = SCRIPT.resolve().parents[2]
        key = sources.git(repo, "rev-parse", "HEAD:Cargo.toml").decode().strip()
        node = {"repository": repo, "entries": [("Cargo.toml", "100644", key)]}
        files, sizes = sources.object_sizes(node)
        rows = list(sources.blobs(node, files, sizes, self.root))
        self.assertEqual(oid(rows[0][3]), key)
        node["entries"] = [("missing", "100644", "0" * 40)]
        with self.assertRaises(ValueError):
            sources.object_sizes(node)

    def test_git_reads_bound_time_and_disable_replacement_and_lazy_fetch(self):
        result = subprocess.CompletedProcess([], 0, stdout=b"result", stderr=b"")
        with patch.object(sources.subprocess, "run", return_value=result) as run:
            self.assertEqual(sources.git(self.root, "cat-file", "--batch"), b"result")
            options = run.call_args.kwargs
            self.assertEqual(options["timeout"], 120)
            self.assertEqual(options["env"]["GIT_NO_REPLACE_OBJECTS"], "1")
            self.assertEqual(options["env"]["GIT_NO_LAZY_FETCH"], "1")
            result.returncode = 128
            with self.assertRaisesRegex(ValueError, "Git cat-file failed"):
                sources.git(self.root, "cat-file", "--batch")


if __name__ == "__main__":
    unittest.main()
