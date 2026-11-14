import errno
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from test_distribution_inputs import inputs


SCRIPT = Path(__file__).with_name("distribution-package.py")
spec = importlib.util.spec_from_file_location("distribution_package", SCRIPT)
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)
EPOCH = 1700000000


def elf(machine, name):
    # Synthetic header fixture: tests CPU/header validation, not executable behavior.
    header = bytearray(64)
    header[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HHI", header, 16, 2, machine, 1)
    struct.pack_into("<H", header, 52, 64)
    return bytes(header) + name.encode("ascii")


def payloads(root):
    for slug, machine in (("linux-amd64", 62), ("linux-arm64", 183)):
        platform = root / slug
        (platform / "bin").mkdir(parents=True)
        (platform / "licenses").mkdir()
        (platform / "README.md").write_text("Synthetic fixture runtime instructions.\n")
        (platform / "licenses" / "MIT.txt").write_text("Synthetic fixture license.\n")
        for name in packaging.distribution.release_plan(inputs())["binaries"]:
            (platform / "bin" / name).write_bytes(elf(machine, name))


class BinaryPackagingTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        payloads(self.source)

    def test_bundles_receipts_and_reproducibility(self):
        output = self.root / "first"
        receipt = packaging.package(inputs(), self.source, output, EPOCH)
        self.assertEqual(len(receipt["archives"]), 6)
        self.assertFalse((output / "INCOMPLETE").exists())
        self.assertEqual(json.loads((output / "binary-manifest.json").read_text()), receipt)
        for name, digest in receipt["archives"].items():
            self.assertEqual(hashlib.sha256((output / name).read_bytes()).hexdigest(), digest)
        with tarfile.open(output / "world-chain-v2.8.3-x86_64-unknown-linux-gnu.tar.gz") as archive:
            self.assertEqual(set(archive.getnames()), {
                "world-chain", "cast", "README.md", "licenses/MIT.txt", "distribution-inputs.json",
            })
            self.assertEqual(archive.extractfile("world-chain").read(), elf(62, "world-chain"))
            self.assertEqual(json.load(archive.extractfile("distribution-inputs.json")), inputs())
            for entry in archive.getmembers():
                self.assertTrue(entry.isfile())
                self.assertEqual((entry.uid, entry.gid, entry.mtime), (0, 0, EPOCH))
                self.assertEqual(entry.mode, 0o755 if entry.name in ("world-chain", "cast") else 0o644)
        with tarfile.open(output / "world-chain-op-v2.8.3-aarch64-unknown-linux-gnu.tar.gz") as archive:
            self.assertNotIn("world-chain", archive.getnames())
            self.assertTrue({"op-node", "op-deployer", "cannon", "kona-host", "forge"} <= set(archive.getnames()))
            self.assertEqual(len(archive.getnames()), 14)
        with tarfile.open(output / "world-chain-combined-v2.8.3-x86_64-unknown-linux-gnu.tar.gz") as archive:
            self.assertEqual(len(archive.getnames()), 15)
        for platform, record in receipt["payloads"].items():
            for relative, digest in record["sha256"].items():
                self.assertEqual(packaging.file_digest(output / record["directory"] / relative), digest)
        for path in self.source.rglob("*"):
            if path.is_file():
                os.utime(path, (EPOCH + 123, EPOCH + 123))
        manifest = self.root / "inputs.json"
        manifest.write_text(json.dumps(inputs()))
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--inputs", str(manifest), "--payload-root", str(self.source),
             "--output", str(self.root / "second"), "--source-date-epoch", str(EPOCH)],
            capture_output=True, text=True, timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        repeated = json.loads((self.root / "second" / "binary-manifest.json").read_text())
        self.assertEqual(receipt, repeated)
        self.assertEqual((output / "SHA256SUMS").read_bytes(), (self.root / "second" / "SHA256SUMS").read_bytes())
        checksum_entries = {name: digest for digest, name in (
            line.split("  ", 1) for line in (output / "SHA256SUMS").read_text().splitlines())}
        self.assertEqual(checksum_entries, receipt["archives"])

    def test_existing_output_is_preserved(self):
        output = self.root / "existing"
        output.mkdir()
        sentinel = output / "keep"
        sentinel.write_text("existing release")
        with self.assertRaises(FileExistsError):
            packaging.package(inputs(), self.source, output, EPOCH)
        self.assertEqual(sentinel.read_text(), "existing release")

    def test_invalid_layout_and_epoch_fail_before_output(self):
        for epoch in (-1, 2**32, True):
            with self.subTest(epoch=epoch), self.assertRaises(ValueError):
                packaging.package(inputs(), self.source, self.root / "bad", epoch)
        extra = self.source / "linux-amd64" / "private.key"
        extra.write_text("not part of the payload")
        with self.assertRaises(ValueError):
            packaging.package(inputs(), self.source, self.root / "bad", EPOCH)
        self.assertFalse((self.root / "bad").exists())
        extra.unlink()
        (self.source / "linux-amd64" / "bin" / "op-node").unlink()
        with self.assertRaises(ValueError):
            packaging.package(inputs(), self.source, self.root / "bad", EPOCH)

    def test_wrong_architecture_does_not_complete(self):
        (self.source / "linux-amd64" / "bin" / "world-chain").write_bytes(elf(183, "wrong CPU"))
        output = self.root / "bad"
        with self.assertRaisesRegex(ValueError, "ELF header for linux/amd64"):
            packaging.package(inputs(), self.source, output, EPOCH)
        self.assertTrue((output / "INCOMPLETE").exists())
        self.assertFalse((output / "binary-manifest.json").exists())

    def test_license_traversal_errors_fail_before_output(self):
        licenses = self.source / "linux-amd64" / "licenses"
        nested = licenses / "dependency"
        nested.mkdir()
        (nested / "NOTICE.txt").write_text("Required dependency notice.\n")
        original_scandir = os.scandir
        for blocked in (licenses, nested):
            with self.subTest(directory=blocked):
                error = PermissionError(errno.EACCES, "Permission denied", str(blocked))

                def scandir(path):
                    if Path(path) == blocked.resolve():
                        raise error
                    return original_scandir(path)

                output = self.root / "unreadable-licenses"
                with patch.object(packaging.os, "scandir", side_effect=scandir):
                    with self.assertRaises(PermissionError) as raised:
                        packaging.package(inputs(), self.source, output, EPOCH)
                self.assertIs(raised.exception, error)
                self.assertFalse(output.exists())

    def test_non_elf_and_unsupported_headers_are_rejected(self):
        for label, contents in (
            ("not-elf", b"MZ" + bytes(64)),
            ("32-bit", elf(62, "wrong class")[:4] + b"\x01" + elf(62, "wrong class")[5:]),
            ("big-endian", elf(62, "wrong endian")[:5] + b"\x02" + elf(62, "wrong endian")[6:]),
        ):
            with self.subTest(label=label):
                (self.source / "linux-amd64" / "bin" / "world-chain").write_bytes(contents)
                with self.assertRaises(ValueError):
                    packaging.package(inputs(), self.source, self.root / label, EPOCH)

    def test_mutating_source_is_rejected(self):
        source = self.source / "linux-amd64" / "README.md"
        original = packaging.os.fstat
        calls = 0

        def observed_stat(descriptor):
            nonlocal calls
            result = original(descriptor)
            if calls == 0:
                os.utime(source, ns=(result.st_atime_ns, result.st_mtime_ns + 1000000000))
            calls += 1
            return result

        with patch.object(packaging.os, "fstat", side_effect=observed_stat):
            with self.assertRaisesRegex(ValueError, "changed while being read"):
                packaging.snapshot_file(source, self.root / "copy.md", False, "linux/amd64")

    def test_symlinks_and_unsafe_license_names_are_rejected(self):
        license_path = self.source / "linux-amd64" / "licenses"
        bad = license_path / "bad name.txt"
        bad.write_text("bad path")
        with self.assertRaises(ValueError):
            packaging.package(inputs(), self.source, self.root / "bad-name", EPOCH)
        bad.unlink()
        (license_path / "elsewhere").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            packaging.package(inputs(), self.source, self.root / "bad-directory", EPOCH)
        (license_path / "elsewhere").unlink()
        executable = self.source / "linux-amd64" / "bin" / "world-chain"
        executable.unlink()
        executable.symlink_to(self.source / "linux-amd64" / "bin" / "op-node")
        with self.assertRaises(OSError):
            packaging.package(inputs(), self.source, self.root / "bad-link", EPOCH)

    def test_output_failure_retains_incomplete_marker(self):
        output = self.root / "failed"
        with patch.object(packaging.tarfile, "open", side_effect=OSError("storage unavailable")):
            with self.assertRaisesRegex(OSError, "storage unavailable"):
                packaging.package(inputs(), self.source, output, EPOCH)
        self.assertTrue((output / "INCOMPLETE").exists())
        self.assertFalse((output / "binary-manifest.json").exists())

    def test_cli_rejects_fifo_without_waiting_for_a_writer(self):
        executable = self.source / "linux-amd64" / "bin" / "world-chain"
        executable.unlink()
        os.mkfifo(executable)
        manifest = self.root / "inputs.json"
        manifest.write_text(json.dumps(inputs()))
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--inputs", str(manifest), "--payload-root", str(self.source),
             "--output", str(self.root / "fifo"), "--source-date-epoch", str(EPOCH)],
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("nonempty regular file", result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
