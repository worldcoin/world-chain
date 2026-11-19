import copy
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

from test_distribution_inputs import component_source_inputs
from test_distribution_package import EPOCH, packaging, payloads


SCRIPT = Path(__file__).with_name("distribution-release.py")
spec = importlib.util.spec_from_file_location("distribution_release", SCRIPT)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


def records(inputs):
    plan = release.distribution.release_plan(inputs)
    build = {"schema_version": 1, "inputs": inputs, "materials": {"fixture": "Synthetic test build records"}}
    images = {"schema_version": 1, "inputs": inputs, "images": {
        variant: {
            "reference": plan["images"][variant]["reference"],
            "index_digest": "sha256:" + str(index + 1) * 64,
            "platforms": {platform: {"manifest_digest": "sha256:" + "a" * 64,
                                     "config_digest": "sha256:" + "b" * 64}
                          for platform in release.distribution.TARGETS},
        } for index, variant in enumerate(release.distribution.VARIANTS)
    }}
    return build, images


class ReleaseFinalizationTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.inputs = component_source_inputs()
        self.assembly = self.root / "assembly"
        source = self.root / "payloads"
        payloads(source)
        self.receipt = packaging.package(self.inputs, source, self.assembly, EPOCH)
        self.build, self.images = records(self.inputs)
        self.sources, self.examples = self.root / "sources.zip", self.root / "examples.zip"
        for path in (self.sources, self.examples):
            self.write_zip(path, self.inputs)
        self.output = self.root / "release"

    def write_zip(self, path, inputs):
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("release-metadata/sources.json", json.dumps({"inputs": inputs}))
            archive.writestr("README.md", "Synthetic test export; no source/runtime qualification.\n")

    def finalize(self):
        return release.finalize(self.assembly, self.sources, self.examples, self.build, self.images, self.output)

    def test_complete_assets_checksums_manifest_and_reproducibility(self):
        (self.assembly / "unrelated-secret.txt").write_text("Synthetic sentinel; must not be copied.")
        manifest = self.finalize()
        self.assertEqual(len(manifest["assets"]), 8)
        self.assertEqual(manifest["inputs"], self.inputs)
        self.assertEqual(manifest["payloads"], self.receipt["payloads"])
        self.assertEqual(manifest["images"], self.images["images"])
        self.assertEqual(manifest["build_materials"], self.build["materials"])
        self.assertNotIn("release-manifest.json", manifest["assets"])
        self.assertEqual(set(p.name for p in self.output.iterdir()),
                         {*manifest["assets"], "SHA256SUMS", "release-manifest.json"})
        sums = {name: digest for digest, name in (
            line.split("  ", 1) for line in (self.output / "SHA256SUMS").read_text().splitlines())}
        self.assertEqual(set(sums), {*manifest["assets"], "release-manifest.json"})
        for name, value in sums.items():
            self.assertEqual(packaging.file_digest(self.output / name), value)
        for name, record in manifest["assets"].items():
            self.assertEqual((self.output / name).stat().st_size, record["bytes"])
            self.assertEqual((self.output / name).stat().st_mode & 0o777, 0o644)
        for name, value in self.receipt["archives"].items():
            self.assertEqual(manifest["assets"][name]["sha256"], value)
        (self.root / "build.json").write_text(json.dumps(self.build))
        (self.root / "images.json").write_text(json.dumps(self.images))
        repeated = self.root / "repeated"
        result = subprocess.run([
            sys.executable, "-B", str(SCRIPT), "--assembly", str(self.assembly),
            "--sources", str(self.sources), "--examples", str(self.examples),
            "--build-record", str(self.root / "build.json"), "--image-record", str(self.root / "images.json"),
            "--output", str(repeated),
        ], capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        for path in self.output.iterdir():
            self.assertEqual(path.read_bytes(), (repeated / path.name).read_bytes())

    def test_tampered_archive_keeps_incomplete_marker(self):
        name = next(iter(self.receipt["archives"]))
        with (self.assembly / name).open("ab") as archive:
            archive.write(b"tampered")
        with self.assertRaisesRegex(ValueError, "archive checksum mismatch"):
            self.finalize()
        self.assertTrue((self.output / "INCOMPLETE").exists())
        self.assertFalse((self.output / "release-manifest.json").exists())
        self.assertFalse((self.output / "SHA256SUMS").exists())

    def test_archive_members_are_checked_independently_of_outer_hash(self):
        plan = release.distribution.release_plan(self.inputs)
        name = plan["assets"]["binaries"]["world-chain"]["linux/amd64"]
        original = copy.deepcopy(self.receipt)
        original["payloads"]["linux/amd64"]["sha256"]["bin/world-chain"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "member checksum mismatch"):
            release.verify_archive(self.assembly / name, original, plan, "linux/amd64", "world-chain")
        for member_name, mode in (("../escape", 0o644), ("README.md", 0o777)):
            bad = self.root / "bad.tar.gz"
            with tarfile.open(bad, "w:gz") as archive:
                info = tarfile.TarInfo(member_name)
                info.mode, info.mtime = mode, EPOCH
                archive.addfile(info)
            with self.subTest(member=member_name), self.assertRaises(ValueError):
                release.verify_archive(bad, self.receipt, plan, "linux/amd64", "world-chain")

    def test_wrong_export_inputs_and_invalid_zips_fail_closed(self):
        wrong = copy.deepcopy(self.inputs)
        wrong["sources"]["world_chain"] = "9" * 40
        for path in (self.sources, self.examples):
            self.write_zip(path, wrong)
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "ZIP metadata"):
                release.verify_zip_inputs(path, self.inputs)
        self.sources.write_bytes(b"not a ZIP")
        with self.assertRaises(zipfile.BadZipFile):
            self.finalize()
        self.assertTrue((self.output / "INCOMPLETE").exists())

    def test_duplicate_and_missing_members_are_rejected(self):
        plan = release.distribution.release_plan(self.inputs)
        for copies, message in ((0, "missing payload"), (2, "repeated")):
            bad = self.root / "bad.tar.gz"
            with tarfile.open(bad, "w:gz") as archive:
                for _ in range(copies):
                    content = b"Synthetic fixture runtime instructions.\n"
                    info = tarfile.TarInfo("README.md")
                    info.mode, info.mtime, info.size = 0o644, EPOCH, len(content)
                    archive.addfile(info, io.BytesIO(content))
            with self.subTest(copies=copies), self.assertRaisesRegex(ValueError, message):
                release.verify_archive(bad, self.receipt, plan, "linux/amd64", "world-chain")

    def test_missing_or_mixed_image_and_build_records_fail_before_output(self):
        plan = release.distribution.release_plan(self.inputs)
        cases = []
        build = copy.deepcopy(self.build)
        build["inputs"]["sources"]["world_chain"] = "8" * 40
        cases.append((build, self.images))
        build = copy.deepcopy(self.build)
        build["materials"] = {}
        cases.append((build, self.images))
        for field, value in (("reference", "example.com/private:test"), ("index_digest", "sha256:invalid")):
            images = copy.deepcopy(self.images)
            images["images"]["op"][field] = value
            cases.append((self.build, images))
        images = copy.deepcopy(self.images)
        del images["images"]["combined"]["platforms"]["linux/arm64"]
        cases.append((self.build, images))
        images = copy.deepcopy(self.images)
        images["images"]["op"]["index_digest"] = images["images"]["combined"]["index_digest"]
        cases.append((self.build, images))
        images = copy.deepcopy(self.images)
        images["schema_version"] = True
        cases.append((self.build, images))
        for index, (build, images) in enumerate(cases):
            with self.subTest(index=index), self.assertRaises(ValueError):
                release.validate_records(self.inputs, plan, build, images)
        self.assertFalse(self.output.exists())

    def test_incomplete_or_existing_output_is_preserved(self):
        marker = self.assembly / "INCOMPLETE"
        marker.write_text("unfinished assembly")
        with self.assertRaisesRegex(ValueError, "incomplete"):
            self.finalize()
        self.assertFalse(self.output.exists())
        marker.unlink()
        self.output.mkdir()
        sentinel = self.output / "keep"
        sentinel.write_text("prior release")
        with self.assertRaises(FileExistsError):
            self.finalize()
        self.assertEqual(sentinel.read_text(), "prior release")

    def test_incomplete_source_export_is_rejected_before_output(self):
        (self.sources.parent / "INCOMPLETE").write_text("source producer failed")
        with self.assertRaisesRegex(ValueError, "input assembly is incomplete"):
            self.finalize()
        self.assertFalse(self.output.exists())

    def test_record_loader_rejects_duplicate_nonfinite_oversized_and_nonobject_json(self):
        path = self.root / "record.json"
        for data in (b'{"x":1,"x":2}', b'{"x":NaN}', b'\xff',
                     b' ' * (release.MAX_RECORD_BYTES + 1), b'[' * 2000 + b']' * 2000):
            with self.subTest(data=data[:20]):
                path.write_bytes(data)
                with self.assertRaises(ValueError):
                    release.load_record(path)


if __name__ == "__main__":
    unittest.main()
