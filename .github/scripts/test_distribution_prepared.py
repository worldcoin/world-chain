import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import textwrap
import unittest
from unittest.mock import patch

import test_distribution_release as finalizer_tests


SCRIPT = Path(__file__).with_name("distribution-prepared.py")
spec = importlib.util.spec_from_file_location("distribution_prepared", SCRIPT)
prepared = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepared)


class PreparedReleaseTest(unittest.TestCase):
    def setUp(self):
        fixture = finalizer_tests.ReleaseFinalizationTest("test_complete_assets_checksums_manifest_and_reproducibility")
        inputs = finalizer_tests.component_source_inputs()
        inputs["bundle_version"] = "v2.9.0"
        with patch.object(finalizer_tests, "component_source_inputs", return_value=inputs):
            fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.manifest = fixture.finalize()
        self.directory = fixture.output
        self.root = fixture.root
        self.source = fixture.inputs["sources"]["world_chain"]
        self.version = fixture.inputs["bundle_version"]
        self.digest = prepared.release.packaging.file_digest(self.directory / "release-manifest.json")
        self.plan = prepared.release.distribution.release_plan(fixture.inputs)
        self.record = {"id": 12, "draft": True, "published_at": None,
                       "tag_name": self.version, "target_commitish": self.source,
                       "assets": [{"id": i + 1, "name": p.name, "size": p.stat().st_size, "state": "uploaded"}
                                  for i, p in enumerate(sorted(self.directory.iterdir()))]}

    def download(self, command, output, limit, timeout):
        identifier = int(command[-1].rsplit("/", 1)[1])
        name = next(x["name"] for x in self.record["assets"] if x["id"] == identifier)
        data = (self.directory / name).read_bytes()
        self.assertLessEqual(len(data), limit)
        self.assertEqual(timeout, 1200)
        output.write(data)

    def prepare(self):
        target = self.root / "prepared"
        with patch.object(prepared, "remote_draft", return_value=self.record), \
                patch.object(prepared, "bounded_command", side_effect=self.download), \
                patch.object(prepared, "verify_images"):
            prepared.prepare("12", self.digest, self.version, self.source, target, "- Release fixture.", "7" * 40)
        return target

    def test_complete_draft_preserves_assets_and_renders_approved_layout(self):
        target = self.prepare()
        self.assertFalse((target / "INCOMPLETE").exists())
        names = (target / "release-files.txt").read_text().splitlines()
        self.assertEqual(len(names), 10)
        self.assertEqual(set(names), {p.name for p in self.directory.iterdir()})
        for name in names:
            self.assertEqual((target / name).read_bytes(), (self.directory / name).read_bytes())
        text = (target / "release-notes.md").read_text()
        headings = ["## Changes", "## Combined image", "## Individual images", "## Binary bundles", "## Source and examples", "## Checksums and provenance"]
        positions = [text.index(x) for x in headings]
        self.assertEqual(positions, sorted(positions))
        for name in self.manifest["assets"]:
            self.assertIn("/releases/download/" + self.version + "/" + name, text)
        self.assertNotIn("{{", text)
        self.assertNotIn("v2.8.3", text)
        self.assertNotIn("Layout preview", text)
        self.assertIn("/blob/" + "7" * 40 + "/docs/distribution-build.md", text)
        self.assertIn("{{TEXT}} and $(command)", prepared.render_page(self.plan, self.source, "- Literal {{TEXT}} and $(command)."))

    def test_wrong_approved_identity_and_published_release_fail(self):
        for key, value in (("draft", False), ("target_commitish", "main"), ("tag_name", "v9.9.9"), ("id", 13)):
            record = dict(self.record, **{key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                prepared.check_draft(record, "12", self.version, self.source)
        with self.assertRaisesRegex(ValueError, "approved SHA-256"):
            prepared.verify_assets(self.directory, "0" * 64, self.version, self.source)
        for version, source in (("v9.9.9", self.source), (self.version, "9" * 40)):
            with self.subTest(version=version, source=source), self.assertRaisesRegex(ValueError, "approved release"):
                prepared.verify_assets(self.directory, self.digest, version, source)

    def test_unexpected_and_duplicate_draft_assets_are_rejected(self):
        extra = {"id": 123, "name": "../../outside", "size": 10, "state": "uploaded"}
        self.record["assets"].append(extra)
        with self.assertRaisesRegex(ValueError, "exactly the planned assets"):
            self.prepare()
        self.assertTrue((self.root / "prepared/INCOMPLETE").exists())
        self.assertFalse((self.root.parent / "outside").exists())
        self.record["assets"][-1] = self.record["assets"][0]
        with patch.object(prepared, "remote_draft", return_value=self.record), self.assertRaisesRegex(ValueError, "duplicate"):
            prepared.prepare("12", self.digest, self.version, self.source, self.root / "another", "Changes", "7" * 40)

    def test_existing_tag_must_resolve_to_the_approved_source(self):
        wrong = {"refs": [{"ref": "refs/tags/" + self.version, "object": {"type": "commit", "sha": "9" * 40}}]}
        with patch.object(prepared, "github_release", return_value=self.record), \
                patch.object(prepared, "command_record", return_value=(b"", wrong)), self.assertRaisesRegex(ValueError, "different source"):
            prepared.remote_draft("12", self.version, self.source)
        tag = {"refs": [{"ref": "refs/tags/" + self.version, "object": {"type": "tag", "sha": "8" * 40}}]}
        commit = {"object": {"type": "commit", "sha": self.source}}
        with patch.object(prepared, "github_release", return_value=self.record), \
                patch.object(prepared, "command_record", side_effect=[(b"", tag), (b"", commit)]):
            self.assertEqual(prepared.remote_draft("12", self.version, self.source), self.record)

    def test_remote_signature_and_asset_readback_rejects_mutation(self):
        for asset in list(self.record["assets"]):
            path = self.directory / (asset["name"] + ".asc")
            path.write_bytes(b"Synthetic detached-signature fixture; no release signature claim.")
            self.record["assets"].append({"id": len(self.record["assets"]) + 1, "name": path.name,
                                          "size": path.stat().st_size, "state": "uploaded"})
        with patch.object(prepared, "remote_draft", return_value=self.record), \
                patch.object(prepared, "bounded_command", side_effect=self.download), patch.object(prepared, "verify_images"):
            prepared.verify_uploaded("12", self.digest, self.version, self.source, self.directory)
        for asset in self.record["assets"]:
            asset["digest"] = "sha256:" + prepared.release.packaging.file_digest(self.directory / asset["name"])
        self.record["assets"][-1]["digest"] = "sha256:" + "f" * 64
        with patch.object(prepared, "remote_draft", return_value=self.record), \
                patch.object(prepared, "bounded_command", side_effect=self.download), self.assertRaisesRegex(ValueError, "uploaded asset digest"):
            prepared.verify_uploaded("12", self.digest, self.version, self.source, self.directory)

    def test_tampered_checksums_members_and_symlinks_fail(self):
        sums = self.directory / "SHA256SUMS"
        original = sums.read_bytes()
        sums.write_bytes(original + original[:1])
        with self.assertRaisesRegex(ValueError, "SHA256SUMS"):
            prepared.verify_assets(self.directory, self.digest, self.version, self.source)
        sums.write_bytes(original)
        name = next(iter(self.manifest["assets"]))
        path = self.directory / name
        data = path.read_bytes()
        path.write_bytes(data + b"tampered")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            prepared.verify_assets(self.directory, self.digest, self.version, self.source)
        path.unlink()
        outside = self.root / "external"
        outside.write_bytes(data)
        path.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "size/type/checksum"):
            prepared.verify_assets(self.directory, self.digest, self.version, self.source)

    def test_wrong_elf_architecture_fails_even_with_matching_receipt_hashes(self):
        name = self.plan["assets"]["binaries"]["world-chain"]["linux/amd64"]
        path = self.directory / name
        with tarfile.open(path, "r:gz") as archive:
            entries = [(member, archive.extractfile(member).read()) for member in archive]
        with tarfile.open(path, "w:gz") as archive:
            for member, data in entries:
                if member.name == "world-chain":
                    data = data[:18] + bytes((183, 0)) + data[20:]
                    self.manifest["payloads"]["linux/amd64"]["sha256"]["bin/world-chain"] = hashlib.sha256(data).hexdigest()
                archive.addfile(member, io.BytesIO(data))
        self.manifest["assets"][name] = {"sha256": prepared.release.packaging.file_digest(path), "bytes": path.stat().st_size}
        manifest_path = self.directory / "release-manifest.json"
        manifest_path.write_bytes(prepared.release.packaging.json_bytes(self.manifest))
        digest = prepared.release.packaging.file_digest(manifest_path)
        sums = {n: r["sha256"] for n, r in self.manifest["assets"].items()}
        sums["release-manifest.json"] = digest
        (self.directory / "SHA256SUMS").write_text("".join(f"{h}  {n}\n" for n, h in sorted(sums.items())))
        with self.assertRaisesRegex(ValueError, "ELF header for linux/amd64"):
            prepared.verify_assets(self.directory, digest, self.version, self.source)


class RemoteBoundsTest(unittest.TestCase):
    def test_command_bytes_deadline_and_http_failure_are_bounded(self):
        output = io.BytesIO()
        prepared.bounded_command([sys.executable, "-c", "print('ok', end='')"], output, 2, 5)
        self.assertEqual(output.getvalue(), b"ok")
        with self.assertRaisesRegex(ValueError, "byte budget"):
            prepared.bounded_command([sys.executable, "-c", "print('long output')"], io.BytesIO(), 2, 5)
        with self.assertRaises(TimeoutError):
            prepared.bounded_command([sys.executable, "-c", "import time; time.sleep(5)"], io.BytesIO(), 100, 0.05)
        with self.assertRaisesRegex(RuntimeError, "HTTP 403"):
            prepared.bounded_command([sys.executable, "-c", "import sys; print('HTTP 403', file=sys.stderr); sys.exit(7)"], io.BytesIO(), 100, 5)


class WorkflowModeTest(unittest.TestCase):
    def test_prepared_mode_and_legacy_cutoff_use_the_real_workflow_guard(self):
        workflow = SCRIPT.parents[1] / "workflows/release.yml"
        text = workflow.read_text()
        guard = re.search(r"python3 - <<'PY'\n(.*?)          PY", text, re.S)
        self.assertIsNotNone(guard)
        program = textwrap.dedent(guard[1])
        for version, identifier, digest, ref, success in (
            ("v2.8.3", "", "", "refs/heads/main", True),
            ("v2.9.0", "", "", "refs/heads/main", False),
            ("v2.9.0", "12", "a" * 64, "refs/heads/main", True),
            ("v2.9.0", "12", "a" * 64, "refs/heads/unreviewed", False),
            ("v2.9.0", "../bad", "a" * 64, "refs/heads/main", False),
        ):
            env = dict(os.environ, INPUT_VERSION=version, PREPARED_RELEASE_ID=identifier,
                       PREPARED_MANIFEST_SHA256=digest, GITHUB_REF=ref, INPUT_REF="1" * 40)
            result = subprocess.run([sys.executable, "-c", program], env=env, capture_output=True, timeout=5)
            with self.subTest(version=version, identifier=identifier, ref=ref):
                self.assertEqual(result.returncode == 0, success, result.stderr.decode())
        env.update(INPUT_VERSION="v2.9.0", PREPARED_RELEASE_ID="12", PREPARED_MANIFEST_SHA256="a" * 64,
                   GITHUB_REF="refs/heads/main", INPUT_REF="main")
        self.assertNotEqual(subprocess.run([sys.executable, "-c", program], env=env, capture_output=True, timeout=5).returncode, 0)
        prepared_job = text.split("  prepared-release:\n", 1)[1].split("\n  prepare-release:\n", 1)[0]
        self.assertIn("environment: release", prepared_job)
        self.assertIn("ref: ${{ github.workflow_sha }}", prepared_job)
        self.assertIn("--verify-uploaded", prepared_job)
        self.assertNotIn("--draft=false", prepared_job)
        self.assertNotIn("docker push", prepared_job)


class ImageBindingTest(unittest.TestCase):
    def setUp(self):
        self.child = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                      "config": {"digest": "sha256:" + "a" * 64}}
        self.raw_child = json.dumps(self.child).encode()
        self.child_sha = "sha256:" + hashlib.sha256(self.raw_child).hexdigest()
        self.index = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json",
                      "manifests": [{"platform": {"os": "linux", "architecture": arch}, "digest": self.child_sha}
                                    for arch in ("amd64", "arm64")]}
        self.image = {"reference": "ghcr.io/worldcoin/world-chain:combined-v2.9.0", "platforms": {
            platform: {"manifest_digest": self.child_sha, "config_digest": self.child["config"]["digest"]}
            for platform in ("linux/amd64", "linux/arm64")}}
        self.pin_index()

    def pin_index(self):
        self.raw_index = json.dumps(self.index).encode()
        self.image["index_digest"] = "sha256:" + hashlib.sha256(self.raw_index).hexdigest()

    def inspect(self, command):
        if command[-1] == self.image["reference"]:
            return self.raw_index, self.index
        self.assertEqual(command[-1], "ghcr.io/worldcoin/world-chain@" + self.child_sha)
        return self.raw_child, self.child

    def test_index_children_and_config_are_verified_with_digest_pinned_children(self):
        with patch.object(prepared, "command_record", side_effect=self.inspect) as call:
            prepared.verify_images({"combined": self.image})
        self.assertEqual(call.call_count, 3)
        self.image["platforms"]["linux/arm64"]["config_digest"] = "sha256:" + "b" * 64
        with patch.object(prepared, "command_record", side_effect=self.inspect), self.assertRaisesRegex(ValueError, "configuration descriptor"):
            prepared.verify_images({"combined": self.image})

    def test_wrong_index_bytes_and_duplicate_platforms_are_rejected(self):
        self.image["index_digest"] = "sha256:" + "c" * 64
        with patch.object(prepared, "command_record", side_effect=self.inspect), self.assertRaisesRegex(ValueError, "OCI bytes"):
            prepared.verify_images({"combined": self.image})
        self.index["manifests"].append(copy.deepcopy(self.index["manifests"][0]))
        self.pin_index()
        with patch.object(prepared, "command_record", side_effect=self.inspect), self.assertRaisesRegex(ValueError, "platform descriptor"):
            prepared.verify_images({"combined": self.image})


if __name__ == "__main__":
    unittest.main()
