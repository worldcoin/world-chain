#!/usr/bin/env python3
"""Read-only WIP1006 game inspection; Python standard library only."""

import argparse
from datetime import datetime, timezone
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request


# First four bytes of keccak256 of the Solidity signatures (not SHA3-256).
# ABI: pkg/contracts/src/dispute/{MultiProofGame.sol,interfaces/IMultiProofGame.sol}.
GETTERS = {
    "gameType": ("bbdc02db", "uint32"),
    "disputeGameFactory": ("f2b4e617", "address"),
    "rootClaim": ("bcef3b55", "bytes32"),
    "extraData": ("609d3334", "bytes"),
    "createdAt": ("cf09e0d0", "uint64"),
    "status": ("200d2ed2", "uint8"),
    "challengerBond": ("68ccdc86", "uint256"),
    "proposerBond": ("d2e25d84", "uint256"),
    "aggregationVKey": ("276fd5ac", "bytes32"),
    "rangeVKeyCommitment": ("9bdd5de1", "bytes32"),
    "teeImageId": ("5f0a6ce0", "bytes32"),
    "validityProofVerifier": ("d020cbb1", "address"),
    "teeVerifier": ("cda752ff", "address"),
    "securityCouncil": ("27eb6c0f", "address"),
    "claimData": ("3ec4d4d6", "claim"),
    "l2SequenceNumber": ("99735e32", "uint256"),
    "gameCreator": ("37b1b229", "address"),
    "l1Head": ("6361506d", "bytes32"),
    "parentRef": ("93f4d416", "address"),
    "attempt": ("732b7a8d", "uint256"),
    "proposalDomainHash": ("0e972f39", "bytes32"),
    "bondVault": ("990826b3", "address"),
    "anchorStateRegistry": ("5c0cba33", "address"),
    "token": ("fc0c546a", "address"),
    "decimals": ("313ce567", "uint8"),
}
STATUSES = ("IN_PROGRESS", "CHALLENGER_WINS", "DEFENDER_WINS")
PROPOSALS = (
    "Unchallenged", "Challenged", "UnchallengedAndValidProofProvided",
    "ChallengedAndValidProofProvided", "Resolved",
)
REASONS = ("NONE", "PROOF_TIMEOUT", "INVALID_PARENT")
LANES = ("VALIDITY_PROOF", "TEE_ATTESTATION", "SECURITY_COUNCIL")


class NotGame(ValueError):
    pass


class Reorg(ValueError):
    pass


class CallError(ValueError):
    def __init__(self, code, reverted=False):
        super().__init__(f"RPC error code {code}" + (" (execution reverted)" if reverted else ""))
        self.reverted = reverted


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
        raise ValueError("Invalid RPC batch response count")
    by_id = {}
    for entry in results:
        if not isinstance(entry, dict):
            raise ValueError("Invalid RPC response entry")
        index = entry.get("id")
        if type(index) is not int or index not in range(len(calls)) or index in by_id:
            raise ValueError("Invalid or duplicate RPC response ID")
        if entry.get("jsonrpc") != "2.0" or ("result" in entry) == ("error" in entry):
            raise ValueError("Invalid RPC response envelope")
        if "error" in entry:
            error = entry["error"]
            if not isinstance(error, dict) or type(error.get("code")) is not int:
                raise ValueError("Invalid RPC error response")
            # Classify reverts, but never expose provider-supplied text/credentials.
            message = error.get("message", "")
            reverted = isinstance(message, str) and "execution reverted" in message.lower()
            by_id[index] = CallError(error["code"], reverted)
        else:
            by_id[index] = entry["result"]
    return [by_id[i] for i in range(len(calls))]


def require(value):
    if isinstance(value, Exception):
        raise value
    return value


def hexdata(value):
    require(value)
    if not isinstance(value, str) or not re.fullmatch(r"0x(?:[0-9a-fA-F]{2})*", value):
        raise ValueError("Invalid hexadecimal data")
    return value[2:].lower()


def quantity(value):
    require(value)
    if not isinstance(value, str) or not re.fullmatch(r"0x(?:0|[1-9a-fA-F][0-9a-fA-F]*)", value):
        raise ValueError("Invalid hexadecimal quantity")
    return int(value, 16)


def decode(value, kind):
    data = hexdata(value)
    if kind == "bytes":
        if len(data) < 128 or int(data[:64], 16) != 32:
            raise ValueError("Invalid dynamic bytes ABI offset")
        size = int(data[64:128], 16)
        if len(data) != 128 + ((size + 31) // 32) * 64 or any(c != "0" for c in data[128 + size * 2:]):
            raise ValueError("Invalid dynamic bytes ABI length/padding")
        return "0x" + data[128:128 + size * 2]
    if kind == "claim":
        if len(data) != 320:
            raise ValueError("Invalid claim ABI length")
        return [decode("0x" + data[i * 64:(i + 1) * 64], item)
                for i, item in enumerate(("uint8", "address", "uint64", "uint8", "uint8"))]
    if len(data) != 64:
        raise ValueError("Expected one ABI word")
    if kind == "bytes32":
        return "0x" + data
    number = int(data, 16)
    bits = 160 if kind == "address" else int(kind[4:])
    if number >= 2**bits:
        raise ValueError(f"ABI value exceeds {kind}")
    return "0x" + data[-40:] if kind == "address" else number


def read(url, block, target, names):
    results = rpc(url, [("eth_call", [{"to": target, "data": "0x" + GETTERS[name][0]}, block])
                        for name in names])
    values = {}
    for name, result in zip(names, results):
        try:
            values[name] = decode(result, GETTERS[name][1])
        except ValueError as error:
            values[name] = error
    return values


def utc(timestamp):
    try:
        return datetime.fromtimestamp(timestamp, timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")
    except (OverflowError, OSError, ValueError):
        return "outside supported UTC date range"


def inspect(url, game, chain_id, expected_factory=None):
    chain, snapshot = rpc(url, [("eth_chainId", []), ("eth_getBlockByNumber", ["latest", False])])
    if quantity(chain) != chain_id:
        raise ValueError("Provider chain ID does not match requested network")
    require(snapshot)
    if not isinstance(snapshot, dict):
        raise ValueError("Snapshot block unavailable")
    block = hex(quantity(snapshot.get("number")))
    block_hash = decode(snapshot.get("hash"), "bytes32")
    now = quantity(snapshot.get("timestamp"))
    code, = rpc(url, [("eth_getCode", [game, block])])
    if not hexdata(code):
        raise NotGame()
    game_type = read(url, block, game, ["gameType"])["gameType"]
    if isinstance(game_type, CallError) and not game_type.reverted:
        raise game_type
    if isinstance(game_type, ValueError) or game_type != 1006:
        raise NotGame()
    identity = read(url, block, game, ["disputeGameFactory", "rootClaim", "extraData", "createdAt"])
    for name, value in identity.items():
        if isinstance(value, Exception):
            raise ValueError(f"Cannot verify {name}: {value}")
    factory = identity["disputeGameFactory"]
    if expected_factory and factory != expected_factory:
        raise NotGame()
    extra = identity["extraData"][2:]
    # ABI encode games(uint32,bytes32,bytes): dynamic tail starts after 3 words.
    args = f"{1006:064x}" + identity["rootClaim"][2:] + f"{96:064x}{len(extra) // 2:064x}"
    args += extra.ljust(((len(extra) + 63) // 64) * 64, "0")
    registration, = rpc(url, [("eth_call", [{"to": factory, "data": "0x5f0150cb" + args}, block])])
    data = hexdata(registration)
    if len(data) != 128:
        raise ValueError("Invalid factory registration ABI")
    registered = decode("0x" + data[:64], "address")
    created = decode("0x" + data[64:], "uint64")
    if registered != game or not created or created != identity["createdAt"]:
        raise NotGame()

    errors, warnings = [], []
    if not expected_factory:
        warnings.append("Type and factory registration verified; canonical deployment provenance not authenticated.")

    def collect(target, names):
        try:
            values = read(url, block, target, names)
        except ValueError as error:
            values = dict.fromkeys(names, error)
        for name, value in values.items():
            if isinstance(value, Exception):
                errors.append(f"{name} at {target}: {value}")
        return {name: value for name, value in values.items() if not isinstance(value, Exception)}

    names = [name for name in GETTERS if name not in identity and name not in ("gameType", "token", "decimals")]
    values = {**identity, **collect(game, names)}
    token, decimals = None, None
    if "bondVault" in values:
        token = collect(values["bondVault"], ["token"]).get("token")
        if token:
            decimals = collect(token, ["decimals"]).get("decimals")

    def enum(value, labels):
        if value >= len(labels):
            warnings.append(f"Unknown enum value {value}")
            return f"UNKNOWN ({value})"
        return f"{labels[value]} ({value})"

    def bond(name):
        if name not in values:
            return "unavailable"
        amount = values[name]
        raw = f"{amount} raw token units"
        if decimals is None:
            return raw
        digits = str(amount).zfill(decimals + 1)
        formatted = (digits[:-decimals] + "." + digits[-decimals:]).rstrip("0").rstrip(".") if decimals else digits
        return f"{formatted} tokens ({raw})"

    fields = {"GameStatus": enum(values["status"], STATUSES) if "status" in values else "unavailable",
              "challengerBond": bond("challengerBond"), "proposerBond": bond("proposerBond")}
    for name in ("aggregationVKey", "rangeVKeyCommitment", "teeImageId", "validityProofVerifier", "teeVerifier", "securityCouncil"):
        fields[name] = values.get(name, "unavailable")
    fields["createdAt"] = f"{created} — {utc(created)}"
    claim_fields = ("status", "challenger", "deadline", "proofBitmap", "invalidationReason")
    if "claimData" in values:
        proposal, challenger, deadline, bitmap, reason = values["claimData"]
        lanes = ", ".join(lane for i, lane in enumerate(LANES) if bitmap & (1 << i)) or "no accepted lanes"
        if bitmap & ~7:
            warnings.append(f"Unknown proof bitmap bits: {hex(bitmap & ~7)}")
        delta = deadline - now
        claim = [enum(proposal, PROPOSALS), challenger + (" (no challenger)" if int(challenger, 16) == 0 else ""),
                 f"{deadline} — {utc(deadline)}; {abs(delta)} seconds {'remaining' if delta > 0 else 'elapsed'} at snapshot",
                 f"0x{bitmap:02x} ({bitmap}) — {lanes}", enum(reason, REASONS)]
    else:
        claim = ["unavailable"] * 5
    fields.update(("claimData." + name, value) for name, value in zip(claim_fields, claim))
    for name in ("rootClaim", "l2SequenceNumber", "gameCreator", "l1Head", "parentRef", "attempt", "proposalDomainHash"):
        fields[name] = str(values.get(name, "unavailable"))
    if "parentRef" in values and "anchorStateRegistry" in values:
        fields["parentRef"] += " (anchor registry)" if values["parentRef"] == values["anchorStateRegistry"] else " (parent game reference)"
    current, = rpc(url, [("eth_getBlockByNumber", [block, False])])
    require(current)
    if not isinstance(current, dict):
        raise ValueError("Snapshot recheck unavailable")
    if decode(current.get("hash"), "bytes32") != block_hash:
        raise Reorg("Snapshot changed during investigation")
    return {
        "game": game, "chain_id": chain_id, "game_type": 1006,
        "snapshot": {"number": int(block, 16), "hash": block_hash, "timestamp": now, "utc": utc(now)},
        "factory": factory, "expected_factory": expected_factory,
        "bond_token": token, "bond_token_decimals": decimals,
        "complete": not errors, "fields": fields, "warnings": warnings, "errors": errors,
    }


def address_arg(value):
    if not re.fullmatch(r"0x[0-9a-fA-F]{40}", value):
        raise argparse.ArgumentTypeError("address must be 0x followed by 40 hexadecimal digits")
    return value.lower()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("address", type=address_arg)
    parser.add_argument("network", nargs="?", choices=("mainnet", "sepolia"), default="mainnet")
    parser.add_argument("--expected-factory", type=address_arg)
    args = parser.parse_args()
    provider, chain_id = (("ETHEREUM_PROVIDER", 1) if args.network == "mainnet"
                          else ("ETHEREUM_SEPOLIA_PROVIDER", 11155111))
    url = os.environ.get(provider)
    if not url:
        parser.error(f"{provider} is not set")
    try:
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme not in ("http", "https") or not parsed.hostname:
            raise ValueError()
    except ValueError:
        parser.error(f"{provider} must be an HTTP(S) URL")
    try:
        for attempt in range(2):
            try:
                report = inspect(url, args.address, chain_id, args.expected_factory)
                break
            except Reorg:
                if attempt:
                    raise ValueError("Snapshot changed twice; rerun when the chain is stable") from None
        report["network"] = args.network
        print(json.dumps(report, indent=2))
        return 0 if report["complete"] else 1
    except NotGame:
        print("This is not a WIP1006 game contract address.")
        return 2
    except ValueError as error:
        print(f"Investigation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
