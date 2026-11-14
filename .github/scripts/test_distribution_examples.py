import json
import os
from pathlib import Path
import subprocess
import unittest


EXAMPLES = Path(__file__).resolve().parents[2] / "examples" / "distribution"


class DeploymentExamplesTest(unittest.TestCase):
    def test_compose_modes_preserve_processes_and_private_rpc(self):
        env = os.environ.copy()
        for name in ("WORLD_CHAIN_ARGS", "OP_NODE_ARGS", "NODE_UID", "NODE_GID"):
            env.pop(name, None)
        env.update(
            COMBINED_IMAGE="example.invalid/world-chain@sha256:" + "1" * 64,
            WORLD_CHAIN_IMAGE="example.invalid/world-chain@sha256:" + "2" * 64,
            OP_IMAGE="example.invalid/op@sha256:" + "3" * 64,
            OP_NODE_L1_ETH_RPC="https://l1.example.invalid",
            OP_NODE_L1_BEACON="https://beacon.example.invalid",
        )
        for mode, config in (
            ("combined", "node.env.example"), ("individual", "node.env.example"),
            ("combined", "node.mainnet.env.example"), ("individual", "node.mainnet.env.example"),
        ):
            with self.subTest(mode=mode, config=config):
                result = subprocess.run(
                    ["docker", "compose", "--env-file", config, "-f",
                     f"compose.{mode}.yaml", "config", "--format", "json"],
                    cwd=EXAMPLES, env=env, capture_output=True, text=True, timeout=20,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                services = json.loads(result.stdout)["services"]
                execution, rollup = services["execution"], services["rollup"]
                self.assertEqual(execution["entrypoint"], ["/usr/local/bin/world-chain"])
                self.assertEqual(rollup["entrypoint"], ["/usr/local/bin/op-node"])
                args = execution["command"]
                self.assertEqual(args[0], "node")
                chain = "worldchain" if "mainnet" in config else "worldchain-sepolia"
                self.assertEqual(args[args.index("--chain") + 1], chain)
                self.assertIn("--network=" + chain, rollup["command"])
                self.assertEqual(args[args.index("--authrpc.addr") + 1], "127.0.0.1")
                self.assertEqual(args[args.index("--http.addr") + 1], "127.0.0.1")
                self.assertNotIn("--builder.enabled", args)
                self.assertIn("--l2=http://127.0.0.1:8551", rollup["command"])
                self.assertNotIn("--sequencer.enabled", rollup["command"])
                for service in (execution, rollup):
                    self.assertEqual(service["network_mode"], "host")
                    self.assertNotIn("ports", service)
                if mode == "combined":
                    self.assertEqual(execution["image"], env["COMBINED_IMAGE"])
                    self.assertEqual(rollup["image"], env["COMBINED_IMAGE"])
                else:
                    self.assertEqual(execution["image"], env["WORLD_CHAIN_IMAGE"])
                    self.assertEqual(rollup["image"], env["OP_IMAGE"])


if __name__ == "__main__":
    unittest.main()
