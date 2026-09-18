#!/usr/bin/env python3
"""Read-only WIP1006 root comparison against finalized L2 outputs."""

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request


BATCH_SIZE = 50
# First four bytes of keccak256 of each Solidity signature.
SELECTORS = {
    "gameCount": "0x4d1975b4",
    "gameAtIndex": "0xbb8aa1fc",
    "challengeDeadline": "0xca593dad",
    "l2SequenceNumber": "0x99735e32",
    "rootClaim": "0xbcef3b55",
    "gameCreator": "0x37b1b229",
    "status": "0x200d2ed2",
    "claimData": "0x3ec4d4d6",
}
STATUSES = ("IN_PROGRESS", "CHALLENGER_WINS", "DEFENDER_WINS")
PROPOSALS = (
    "Unchallenged", "Challenged", "UnchallengedAndValidProofProvided",
    "ChallengedAndValidProofProvided", "Resolved",
)


def rpc(url, calls, *, batch=True):
    payload = [
        {"jsonrpc": "2.0", "id": i, "method": method, "params": params}
        for i, (method, params) in enumerate(calls)
    ]
    try:
        request = urllib.request.Request(
            url, data=json.dumps(payload if batch else payload[0]).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=20) as response:
            results = json.load(response)
    except urllib.error.HTTPError as error:
        raise ValueError(f"RPC HTTP failure: status {error.code}") from None
    except (urllib.error.URLError, TimeoutError, OSError):
        raise ValueError("RPC connection failed or timed out") from None
    except (ValueError, UnicodeError):
        raise ValueError("Invalid RPC URL or JSON response") from None
    if not batch:
        results = [results]
    if not isinstance(results, list) or len(results) != len(calls):
        raise ValueError("RPC returned an invalid response count")
    by_id = {}
    for result in results:
        if not isinstance(result, dict):
            raise ValueError("RPC returned an invalid response entry")
        index = result.get("id")
        if type(index) is not int or index not in range(len(calls)) or index in by_id:
            raise ValueError("RPC returned invalid or duplicate response IDs")
        if result.get("jsonrpc") != "2.0" or "error" in result or "result" not in result:
            # Provider error text may contain URLs or credentials.
            raise ValueError(f"RPC {calls[index][0]} failed (item {index})")
        by_id[index] = result["result"]
    return [by_id[i] for i in range(len(calls))]


def quantity(value):
    if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]+", value):
        raise ValueError("Invalid hexadecimal quantity")
    return int(value, 16)


def number(value):
    if type(value) is int and value >= 0:
        return value
    return quantity(value)


def hash32(value):
    if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]{64}", value):
        raise ValueError("Invalid 32-byte hash")
    return value.lower()


def words(value, count):
    if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]{%d}" % (64 * count), value):
        raise ValueError(f"Invalid ABI response: expected {count} words")
    return [int(value[i:i + 64], 16) for i in range(2, len(value), 64)]


def address(value):
    if not 0 <= value < 2**160:
        raise ValueError("Invalid ABI address")
    return f"0x{value:040x}"


def contract_calls(url, block, requests):
    results = []
    for start in range(0, len(requests), BATCH_SIZE):
        chunk = requests[start:start + BATCH_SIZE]
        calls = [
            ("eth_call", [{"to": target, "data": SELECTORS[method] + args}, block])
            for target, method, args in chunk
        ]
        try:
            results.extend(rpc(url, calls))
        except ValueError as error:
            target, method, _ = chunk[0]
            raise ValueError(
                f"L1 batch starting with {method} at {target}: {error}"
            ) from None
    return results


def scan(l1_url, l2_url, factory, chain_id):
    chain, snapshot = rpc(l1_url, [
        ("eth_chainId", []), ("eth_getBlockByNumber", ["latest", False]),
    ])
    if quantity(chain) != chain_id:
        raise ValueError("L1 chain ID does not match the requested network")
    if not isinstance(snapshot, dict):
        raise ValueError("L1 snapshot unavailable")
    block = hex(quantity(snapshot.get("number")))
    block_hash = hash32(snapshot.get("hash"))
    now = quantity(snapshot.get("timestamp"))
    sync, = rpc(l2_url, [("optimism_syncStatus", [])], batch=False)
    if not isinstance(sync, dict) or not isinstance(sync.get("finalized_l2"), dict):
        raise ValueError("L2 finalized head unavailable")
    finalized = number(sync["finalized_l2"].get("number"))
    finalized_hash = hash32(sync["finalized_l2"].get("hash"))
    raw_count, = contract_calls(l1_url, block, [(factory, "gameCount", "")])
    count, = words(raw_count, 1)
    report = {
        "l1_chain_id": chain_id, "l1_block_number": int(block, 16),
        "l1_block_hash": block_hash, "l1_timestamp": now,
        "factory": factory, "finalized_l2_number": finalized,
        "finalized_l2_hash": finalized_hash, "factory_game_count": count,
        "factory_entries_read": 0, "games_inside_deadline": 0,
        "skipped_unfinalized": 0, "games_compared": 0,
        "distinct_outputs_requested": 0, "mismatches": [],
    }
    roots = {}
    stop = False
    for end in range(count, 0, -BATCH_SIZE):
        indexes = list(range(end - 1, max(-1, end - BATCH_SIZE - 1), -1))
        entries = contract_calls(l1_url, block, [
            (factory, "gameAtIndex", f"{index:064x}") for index in indexes
        ])
        report["factory_entries_read"] += len(entries)
        games = []
        for index, entry in zip(indexes, entries):
            game_type, created_at, proxy = words(entry, 3)
            if game_type >= 2**32 or created_at >= 2**64 or proxy == 0:
                raise ValueError(f"Invalid factory entry at index {index}")
            game_address = address(proxy)
            if game_type == 1006:
                games.append((index, game_address))
        deadlines = contract_calls(l1_url, block, [
            (game, "challengeDeadline", "") for _, game in games
        ])
        for (index, game), raw_deadline in zip(games, deadlines):
            deadline, = words(raw_deadline, 1)
            if deadline >= 2**64:
                raise ValueError(f"Invalid challenge deadline at {game}")
            # Deadlines are monotonic across WIP1006 factory indexes.
            if deadline <= now:
                stop = True
                break
            report["games_inside_deadline"] += 1
            try:
                raw_height, raw_root = contract_calls(l1_url, block, [
                    (game, "l2SequenceNumber", ""), (game, "rootClaim", ""),
                ])
                height, = words(raw_height, 1)
                claimed = hash32(raw_root)
                if height > finalized:
                    report["skipped_unfinalized"] += 1
                    continue
                if height not in roots:
                    output, = rpc(l2_url, [
                        ("optimism_outputAtBlock", [hex(height)]),
                    ], batch=False)
                    if not isinstance(output, dict) or not isinstance(output.get("blockRef"), dict):
                        raise ValueError("Invalid L2 output response")
                    if number(output["blockRef"].get("number")) != height:
                        raise ValueError("L2 output returned a different block number")
                    if height == finalized and hash32(output["blockRef"].get("hash")) != finalized_hash:
                        raise ValueError("L2 output disagrees with the finalized head hash")
                    roots[height] = hash32(output.get("outputRoot"))
                report["games_compared"] += 1
                if roots[height] == claimed:
                    continue
                creator_raw, status_raw, claim_raw = contract_calls(l1_url, block, [
                    (game, "gameCreator", ""), (game, "status", ""),
                    (game, "claimData", ""),
                ])
                creator, = words(creator_raw, 1)
                status, = words(status_raw, 1)
                proposal, challenger, proof_deadline, bitmap, reason = words(claim_raw, 5)
                if (status >= len(STATUSES) or proposal >= len(PROPOSALS)
                        or proof_deadline >= 2**64 or bitmap >= 2**8 or reason > 2):
                    raise ValueError("Invalid game status or claim data")
                report["mismatches"].append({
                    "game": game, "factory_index": index, "l2_block_number": height,
                    "root_claim": claimed, "expected_root": roots[height],
                    "creator": address(creator), "challenged": challenger != 0,
                    "challenger": address(challenger), "status": STATUSES[status],
                    "proposal_status": PROPOSALS[proposal], "challenge_deadline": deadline,
                })
            except ValueError as error:
                raise ValueError(f"game={game} index={index}: {error}") from None
        if stop:
            break
    current, = rpc(l1_url, [("eth_getBlockByNumber", [block, False])])
    if not isinstance(current, dict) or hash32(current.get("hash")) != block_hash:
        raise ValueError("L1 snapshot changed during scan; rerun the scan")
    report["distinct_outputs_requested"] = len(roots)
    report["mismatches"].sort(key=lambda game: game["factory_index"])
    return report


def valid_url(value):
    try:
        parsed = urllib.parse.urlsplit(value)
        return parsed.scheme in ("http", "https") and bool(parsed.hostname)
    except ValueError:
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("factory", help="dispute game factory address")
    parser.add_argument("l2_consensus_rpc", help="HTTP(S) Optimism consensus RPC URL")
    parser.add_argument("network", nargs="?", choices=("mainnet", "sepolia"), default="mainnet")
    args = parser.parse_args()
    if not re.fullmatch(r"0x[0-9a-fA-F]{40}", args.factory) or int(args.factory, 16) == 0:
        parser.error("factory must be a nonzero 20-byte hexadecimal address")
    provider, chain_id = (("ETHEREUM_PROVIDER", 1) if args.network == "mainnet"
                          else ("ETHEREUM_SEPOLIA_PROVIDER", 11155111))
    l1_url = os.environ.get(provider)
    if not l1_url or not valid_url(l1_url):
        parser.error(f"{provider} must be set to an HTTP(S) URL")
    if not valid_url(args.l2_consensus_rpc):
        parser.error("L2 consensus RPC must be an HTTP(S) URL")
    try:
        report = scan(l1_url, args.l2_consensus_rpc, args.factory.lower(), chain_id)
    except ValueError as error:
        print(f"Scan failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
