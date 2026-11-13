#!/usr/bin/env python3
"""Find the nearest ASR claim-valid defender-winning ancestor; read-only RPC."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import sys
from urllib.parse import urlsplit


HELPER = Path(__file__).resolve().parents[2] / "investigate-game/scripts/investigate_game.py"
spec = importlib.util.spec_from_file_location("game_rpc", HELPER)
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
ZERO = "0x" + "00" * 20
NETWORKS = {"mainnet": (1, "ETHEREUM_PROVIDER"), "sepolia": (11155111, "ETHEREUM_SEPOLIA_PROVIDER")}
MAX_PARENT_HOPS = 200
# First four bytes of Keccak-256 of the Solidity signatures.
REGISTRY_GETTERS = {"isGameRegistered": "ee658e45", "isGameClaimValid": "6c4f4467"}


class NoRecoveryParent(ValueError):
    pass


def address_arg(value):
    address = base.address_arg(value)
    if address == ZERO:
        raise argparse.ArgumentTypeError("game/factory address must be nonzero")
    return address


def registry_bool(url, block, registry, getter, game):
    data = "0x" + REGISTRY_GETTERS[getter] + game[2:].rjust(64, "0")
    result, = base.rpc(url, [("eth_call", [{"to": registry, "data": data}, block])])
    value = base.decode(result, "uint8")
    if value not in (0, 1):
        raise ValueError(f"Invalid ABI boolean from {getter}")
    return bool(value)


def read(url, block, game, names):
    values = base.read(url, block, game, names)
    for name, value in values.items():
        if isinstance(value, Exception):
            raise ValueError(f"Cannot read {name} at {game}: {value}")
    return values


def verify_snapshot(url, number, block_hash):
    current, = base.rpc(url, [("eth_getBlockByNumber", [hex(number), False])])
    base.require(current)
    if not isinstance(current, dict):
        raise ValueError("Cannot verify snapshot block")
    if base.decode(current.get("hash"), "bytes32") != block_hash:
        raise base.Reorg("Snapshot changed during traversal")


def find_parent(url, game, chain_id, expected_factory=None):
    chain, snapshot = base.rpc(url, [
        ("eth_chainId", []), ("eth_getBlockByNumber", ["latest", False]),
    ])
    if base.quantity(chain) != chain_id:
        raise ValueError("Provider chain ID does not match requested network")
    base.require(snapshot)
    if not isinstance(snapshot, dict):
        raise ValueError("Snapshot block unavailable")
    number = base.quantity(snapshot.get("number"))
    block_hash = base.decode(snapshot.get("hash"), "bytes32")
    timestamp = base.quantity(snapshot.get("timestamp"))
    block = {"blockHash": block_hash, "requireCanonical": True}
    identity = read(url, block, game, ["gameType", "anchorStateRegistry", "disputeGameFactory"])
    if identity["gameType"] != 1006:
        raise ValueError("Initial address is not a WIP-1006 game")
    registry, factory = identity["anchorStateRegistry"], identity["disputeGameFactory"]
    if registry == ZERO or factory == ZERO:
        raise ValueError("Initial game has a zero registry or factory")
    registry_factory = read(url, block, registry, ["disputeGameFactory"])["disputeGameFactory"]
    if registry_factory != factory or (expected_factory and factory != expected_factory):
        raise ValueError("Game factory does not match the registry or expected deployment")
    if not registry_bool(url, block, registry, "isGameRegistered", game):
        raise ValueError("Initial game is not registered in the registry's factory")

    visited = set()
    for hops in range(MAX_PARENT_HOPS + 1):
        if game == registry:
            verify_snapshot(url, number, block_hash)
            raise NoRecoveryParent("Reached the ASR sentinel without a claim-valid defender-winning game")
        if game == ZERO or game in visited:
            raise ValueError(f"Zero parent or lineage cycle at {game}")
        visited.add(game)
        status = read(url, block, game, ["status"])["status"]
        if status >= len(base.STATUSES):
            raise ValueError(f"Unknown game status at {game}")
        if status == 2 and registry_bool(url, block, registry, "isGameClaimValid", game):
            l2_block = read(url, block, game, ["l2SequenceNumber"])["l2SequenceNumber"]
            if l2_block >= 2**64:
                raise ValueError(f"L2 block number exceeds uint64 at {game}")
            verify_snapshot(url, number, block_hash)
            return {
                "recovery_parent": game, "l2_block_number": l2_block, "parent_hops": hops,
                "status": "DEFENDER_WINS", "is_game_claim_valid": True,
                "registry": registry, "factory": factory,
                "snapshot": {"number": number, "hash": block_hash, "utc": base.utc(timestamp)},
                "warnings": [] if expected_factory else [
                    "Registry/factory wiring verified; canonical deployment provenance not authenticated.",
                ],
            }
        if hops == MAX_PARENT_HOPS:
            raise ValueError(f"{MAX_PARENT_HOPS}-hop limit reached without a recovery parent")
        game = read(url, block, game, ["parentRef"])["parentRef"]
    raise ValueError("Traversal ended unexpectedly")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("address", type=address_arg)
    parser.add_argument("network", choices=NETWORKS)
    parser.add_argument("--expected-factory", type=address_arg)
    args = parser.parse_args()
    chain_id, variable = NETWORKS[args.network]
    url = os.environ.get(variable)
    if not url:
        parser.error(f"{variable} is not set")
    try:
        parsed = urlsplit(url)
        if parsed.scheme not in ("http", "https") or not parsed.hostname:
            raise ValueError()
    except ValueError:
        parser.error(f"{variable} must be an HTTP(S) URL")
    try:
        for attempt in range(2):
            try:
                report = find_parent(url, args.address, chain_id, args.expected_factory)
                break
            except base.Reorg:
                if attempt:
                    raise ValueError("Snapshot changed twice; rerun when the chain is stable") from None
        report["network"] = args.network
        print(json.dumps(report, indent=2))
        return 0
    except NoRecoveryParent as error:
        print(f"No recovery parent: {error}", file=sys.stderr)
        return 2
    except ValueError as error:
        print(f"Recovery lookup failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
