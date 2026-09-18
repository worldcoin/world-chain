#!/usr/bin/env python3
"""Read-only WIP1006 parent traversal; no third-party dependencies."""

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request


STATUSES = ("IN_PROGRESS", "CHALLENGER_WINS", "DEFENDER_WINS")
REASONS = ("NONE", "PROOF_TIMEOUT", "INVALID_PARENT")
# keccak256 of status(), claimData(), and parentRef(), truncated to four bytes.
SELECTORS = ("0x200d2ed2", "0x3ec4d4d6", "0x93f4d416")


def rpc(url, calls):
    payload = [
        {"jsonrpc": "2.0", "id": i, "method": method, "params": params}
        for i, (method, params) in enumerate(calls)
    ]
    try:
        request = urllib.request.Request(
            url, data=json.dumps(payload).encode(),
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

    if not isinstance(results, list) or len(results) != len(calls):
        raise ValueError("RPC returned an invalid batch response")
    by_id = {}
    for result in results:
        if not isinstance(result, dict):
            raise ValueError("RPC returned an invalid response entry")
        index = result.get("id")
        if type(index) is not int or index not in range(len(calls)) or index in by_id:
            raise ValueError("RPC returned invalid or duplicate response IDs")
        if result.get("jsonrpc") != "2.0" or "error" in result or "result" not in result:
            # Provider error messages can contain credentials or request URLs.
            raise ValueError(f"RPC {calls[index][0]} failed (batch item {index})")
        by_id[index] = result["result"]
    return [by_id[i] for i in range(len(calls))]


def quantity(value):
    if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]+", value):
        raise ValueError("RPC returned an invalid hexadecimal quantity")
    return int(value, 16)


def words(value, count):
    if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]{%d}" % (64 * count), value):
        raise ValueError(f"Invalid ABI response: expected {count} words")
    return [int(value[i:i + 64], 16) for i in range(2, len(value), 64)]


def trace(url, address, chain_id):
    chain, block = rpc(url, [("eth_chainId", []), ("eth_blockNumber", [])])
    if quantity(chain) != chain_id:
        raise ValueError("Provider chain ID does not match the requested network")
    block = hex(quantity(block))
    print(f"chain_id={chain_id} block={int(block, 16)}", flush=True)
    visited = set()
    for depth in range(201):
        if address in visited:
            raise ValueError(f"Parent cycle detected at {address}")
        visited.add(address)
        try:
            status_raw, claim_raw, parent_raw = rpc(url, [
                ("eth_call", [{"to": address, "data": selector}, block])
                for selector in SELECTORS
            ])
            status, = words(status_raw, 1)
            proposal, challenger, deadline, bitmap, reason = words(claim_raw, 5)
            if (status >= len(STATUSES) or reason >= len(REASONS) or proposal > 4
                    or challenger >= 2**160 or deadline >= 2**64 or bitmap >= 2**8):
                raise ValueError("Invalid claim/status ABI values")
            print(f"hop={depth} game={address} status={STATUSES[status]} "
                  f"reason={REASONS[reason]}", flush=True)
            if depth == 0 and (status != 1 or reason != 2):
                raise ValueError("Initial game is not CHALLENGER_WINS / INVALID_PARENT")
            if status != 1 or reason != 2:
                print(f"terminal={address} parent_hops={depth}", flush=True)
                return
            parent, = words(parent_raw, 1)
            if not 0 < parent < 2**160:
                raise ValueError("Invalid or zero parent address")
            address = f"0x{parent:040x}"
        except ValueError as error:
            raise ValueError(f"hop={depth} game={address}: {error}") from None
    raise ValueError("200-hop limit reached without a terminal ancestor")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("address")
    parser.add_argument("network", nargs="?", choices=("mainnet", "sepolia"), default="mainnet")
    args = parser.parse_args()
    if not re.fullmatch(r"0x[0-9a-fA-F]{40}", args.address):
        parser.error("game address must be 0x followed by 40 hexadecimal digits")
    provider, chain_id = (("ETHEREUM_PROVIDER", 1) if args.network == "mainnet"
                          else ("ETHEREUM_SEPOLIA_PROVIDER", 11155111))
    url = os.environ.get(provider)
    if not url:
        parser.error(f"{provider} is not set")
    if not url.startswith(("http://", "https://")):
        parser.error(f"{provider} must be an HTTP(S) URL")
    try:
        trace(url, args.address.lower(), chain_id)
    except ValueError as error:
        print(f"Trace failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
