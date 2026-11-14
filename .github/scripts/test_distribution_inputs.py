from copy import deepcopy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("distribution-inputs.py")
spec = importlib.util.spec_from_file_location("distribution_inputs", SCRIPT)
distribution = importlib.util.module_from_spec(spec)
spec.loader.exec_module(distribution)


def inputs():
    return {
        "schema_version": 1,
        "bundle_version": "v2.8.3",
        "sources": {"world_chain": "1" * 40, "optimism": "2" * 40},
        "components": {
            name: {"version": "v1.2.3-worldchain", "upstream_tag": f"{name}/v1.2.3", "upstream_commit": "3" * 40}
            for name in distribution.COMPONENTS
        },
        "platforms": ["linux/amd64", "linux/arm64"],
    }


class DistributionInputsTest(unittest.TestCase):
    def test_asset_identity_and_required_helpers(self):
        value = inputs()
        original = deepcopy(value)
        plan = distribution.release_plan(value)
        self.assertEqual(value, original)
        self.assertEqual(plan["image_binary_directory"], "/usr/local/bin")
        self.assertEqual(plan["assets"]["binaries"]["linux/amd64"], "world-chain-v2.8.3-x86_64-unknown-linux-gnu.tar.gz")
        self.assertEqual(plan["assets"]["sources"], "world-chain-v2.8.3-sources.tar.gz")
        self.assertEqual(plan["assets"]["binaries"]["linux/arm64"], "world-chain-v2.8.3-aarch64-unknown-linux-gnu.tar.gz")
        self.assertEqual(set(plan["binaries"]), {
            "world-chain", "op-node", "op-batcher", "op-proposer", "op-conductor",
            "op-challenger", "op-deployer", "op-dispute-mon", "cannon", "kona-host",
            "forge", "cast",
        })

    def test_independent_component_versions(self):
        value = inputs()
        value["components"]["op-node"].update(version="v1.19.11-worldchain.2", upstream_tag="op-node/v1.19.11")
        plan = distribution.release_plan(value)
        self.assertEqual(plan["inputs"]["components"]["op-node"]["version"], "v1.19.11-worldchain.2")
        self.assertIn("v2.8.3", plan["assets"]["sources"])
        value["bundle_version"] = "v2.8.3-rc.1"
        self.assertIn("v2.8.3-rc.1", distribution.release_plan(value)["assets"]["sources"])

    def test_reject_invalid_contract(self):
        cases = [
            ("schema_version", True), ("schema_version", 2),
            ("bundle_version", "v02.8.3"), ("bundle_version", "v2.8.3;echo bad"),
            ("bundle_version", "v2.8.3\n"), ("bundle_version", "v2.8.3/extra"),
            ("platforms", ["linux/amd64", "linux/amd64"]),
            ("platforms", ["linux/amd64"]), ("platforms", [[], "linux/arm64"]),
            ("sources", {"world_chain": "main", "optimism": "2" * 40}),
            ("sources", {"world_chain": "0" * 40, "optimism": "2" * 40}),
            ("sources", {"world_chain": "1" * 40, "optimism": "A" * 40}),
            ("components", {}),
        ]
        for field, invalid in cases:
            with self.subTest(field=field, invalid=invalid):
                value = inputs()
                value[field] = invalid
                with self.assertRaises(ValueError):
                    distribution.release_plan(value)
        for invalid in ([], None, {**inputs(), "unknown": "ignored?"}):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                distribution.release_plan(invalid)

    def test_reject_invalid_component_binding(self):
        for field, invalid in (
            ("version", "v1.2.3-worldchain.0"), ("version", "v1.2.3-worldchain.01"),
            ("version", "v1.2.3-worldchain+metadata"), ("version", "v01.2.3-worldchain"),
            ("version", 123), ("upstream_tag", "op-batcher/v1.2.3"),
            ("upstream_tag", "op-node/v1.2.4"), ("upstream_commit", "abc123"),
        ):
            with self.subTest(field=field, invalid=invalid):
                value = inputs()
                value["components"]["op-node"][field] = invalid
                with self.assertRaises(ValueError):
                    distribution.release_plan(value)
        for name in distribution.COMPONENTS:
            with self.subTest(missing=name):
                value = inputs()
                del value["components"][name]
                with self.assertRaises(ValueError):
                    distribution.release_plan(value)
        for location in ("sources", "components"):
            value = inputs()
            value[location]["unexpected"] = "ignored?"
            with self.subTest(location=location), self.assertRaises(ValueError):
                distribution.release_plan(value)
        value = inputs()
        value["components"]["op-node"]["unexpected"] = "ignored?"
        with self.assertRaises(ValueError):
            distribution.release_plan(value)

    def test_json_and_command_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "inputs.json"
            path.write_text(json.dumps(inputs()))
            result = subprocess.run([sys.executable, str(SCRIPT), str(path)], capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), distribution.release_plan(inputs()))
            for invalid in (
                b"{", b"\xff", json.dumps(inputs()).encode("utf-16"),
                b'{"schema_version":1,"schema_version":1}',
                b"[" * 2000 + b"]" * 2000,
                b" " * (distribution.MAX_INPUT_BYTES + 1),
            ):
                with self.subTest(invalid=invalid[:40]):
                    path.write_bytes(invalid)
                    result = subprocess.run([sys.executable, str(SCRIPT), str(path)], capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 1)
                    self.assertEqual(result.stdout, "")
                    self.assertTrue(result.stderr.startswith("distribution inputs:"), result.stderr)
            path.unlink()
            result = subprocess.run([sys.executable, str(SCRIPT), str(path)], capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
