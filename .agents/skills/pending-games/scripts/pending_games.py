#!/usr/bin/env python3
"""List unresolved WIP1006 games among the newest 2000 factory entries."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
from urllib.parse import urlsplit


HELPER = Path(__file__).resolve().parents[2] / "investigate-game/scripts/investigate_game.py"
spec = importlib.util.spec_from_file_location("game_rpc", HELPER)
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
BATCH_SIZE = 50
MAX_SCAN = 2000
GAME_TYPE = 1006
NETWORKS = {"mainnet": (1, "ETHEREUM_PROVIDER"), "sepolia": (11155111, "ETHEREUM_SEPOLIA_PROVIDER")}
# First four bytes of keccak256 of the Solidity signatures.
SELECTORS = {"gameCount": "4d1975b4", "gameAtIndex": "bb8aa1fc", "status": "200d2ed2"}


def contract_calls(url, block, requests):
    results = base.rpc(url, [
        ("eth_call", [{"to": target, "data": "0x" + SELECTORS[method] + args}, block])
        for target, method, args in requests
    ])
    for (target, method, _), result in zip(requests, results):
        try:
            base.require(result)
        except ValueError as error:
            raise ValueError(f"{method} at {target}: {error}") from None
    return results


def factory_entry(raw, index):
    data = base.hexdata(raw)
    if len(data) != 192:
        raise ValueError(f"Invalid factory entry ABI length at index {index}")
    game_type, created_at, game = [
        base.decode("0x" + data[i * 64:(i + 1) * 64], kind)
        for i, kind in enumerate(("uint32", "uint64", "address"))
    ]
    if int(game, 16) == 0:
        raise ValueError(f"Zero game address at factory index {index}")
    return game_type, game


def scan(url, factory, chain_id):
    chain, snapshot = base.rpc(url, [
        ("eth_chainId", []), ("eth_getBlockByNumber", ["latest", False]),
    ])
    if base.quantity(chain) != chain_id:
        raise ValueError("L1 chain ID does not match the requested network")
    base.require(snapshot)
    if not isinstance(snapshot, dict):
        raise ValueError("L1 snapshot unavailable")
    number = base.quantity(snapshot.get("number"))
    block_hash = base.decode(snapshot.get("hash"), "bytes32")
    timestamp = base.quantity(snapshot.get("timestamp"))
    block = {"blockHash": block_hash, "requireCanonical": True}
    raw_count, = contract_calls(url, block, [(factory, "gameCount", "")])
    count = base.decode(raw_count, "uint256")
    scan_count = min(count, MAX_SCAN)
    lowest_index = count - scan_count
    pending = []
    games_read = 0
    for end in range(count, lowest_index, -BATCH_SIZE):
        indexes = range(end - 1, max(lowest_index, end - BATCH_SIZE) - 1, -1)
        entries = contract_calls(url, block, [
            (factory, "gameAtIndex", f"{index:064x}") for index in indexes
        ])
        games = []
        for index, raw in zip(indexes, entries):
            try:
                game_type, game = factory_entry(raw, index)
            except ValueError as error:
                raise ValueError(f"Factory index {index}: {error}") from None
            if game_type == GAME_TYPE:
                games.append(game)
        if not games:
            continue
        statuses = contract_calls(url, block, [(game, "status", "") for game in games])
        for game, raw in zip(games, statuses):
            try:
                status = base.decode(raw, "uint8")
                if status >= len(base.STATUSES):
                    raise ValueError(f"Unknown GameStatus {status}")
            except ValueError as error:
                raise ValueError(f"status at {game}: {error}") from None
            if status == 0:
                pending.append(game)
        games_read += len(games)
    current, = base.rpc(url, [("eth_getBlockByNumber", [hex(number), False])])
    base.require(current)
    if not isinstance(current, dict) or base.decode(current.get("hash"), "bytes32") != block_hash:
        raise ValueError("L1 snapshot changed during scan; rerun the scan")
    return {
        "l1_chain_id": chain_id, "l1_block_number": number,
        "l1_block_hash": block_hash, "l1_timestamp": timestamp,
        "factory": factory, "factory_game_count": count,
        "factory_entries_read": scan_count, "wip1006_games_read": games_read,
        "scan_limit": MAX_SCAN, "older_entries_unscanned": lowest_index,
        "pending_count": len(pending), "pending_games": pending,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("factory", help="dispute game factory address")
    parser.add_argument("network", nargs="?", choices=NETWORKS, default="mainnet")
    args = parser.parse_args()
    if not re.fullmatch(r"0x[0-9a-fA-F]{40}", args.factory) or int(args.factory, 16) == 0:
        parser.error("factory must be a nonzero 20-byte hexadecimal address")
    chain_id, provider = NETWORKS[args.network]
    url = os.environ.get(provider, "")
    try:
        parsed = urlsplit(url)
        valid_url = parsed.scheme in ("http", "https") and bool(parsed.hostname)
    except ValueError:
        valid_url = False
    if not valid_url:
        parser.error(f"{provider} must be set to an HTTP(S) URL")
    try:
        report = scan(url, args.factory.lower(), chain_id)
    except ValueError as error:
        print(f"Scan failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
