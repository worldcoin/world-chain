#!/usr/bin/env python3
"""Prepare Safe calls or propose an atomic WIP-1006 council transaction; never execute."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

ROOT = Path(__file__).resolve().parents[1]
ZERO = "0x" + "00" * 20

# Sepolia entries from safe-global/safe-deployments at 7b1fb6d615ab2d2999550ec9166554b180e813e5.
# Delegatecall targets must match both the Safe version and the deployed code hash.
LIBRARIES = {
    "1.3.0": {
        "signMessage": ("0x98FFBBF51bb33A056B08ddf711f289936AafF717", "0x3ac65dea3cc9dd0d7b7b800f834e3d73415b4e944bb94555c3e4a08fb137e918"),
        "multiSend": ("0x998739BFdAAdde7C933B942a68053933098f9EDa", "0x81db0e4afdf5178583537b58c5ad403bd47a4ac7f9bde2442ef3e341d433126a"),
    },
    "1.4.1": {
        "signMessage": ("0xd53cd0aB83D845Ac265BE939c57F53AD838012c9", "0x525c754a46b79e05543a59bb61e8de3c9eee0d955a59352409cbe67ea1077528"),
        "multiSend": ("0x38869bf66a61cF6bDB996A6aE40D5853Fd43B526", "0x0e4f7fc66550a322d1e7688e181b75e217e662a4f3f4d6a29b22bc61217c4b77"),
    },
    "1.5.0": {
        "signMessage": ("0x4FfeF8222648872B3dE295Ba1e49110E61f5b5aa", "0xd61840855da008da59a00fc03fb71455b4f70bdca1f56f9504f072ed8d90c50e"),
        "multiSend": ("0x218543288004CD07832472D464648173c77D7eB7", "0xca1147a12963172a93910c5cb2bfa5ad0e941c7f03fc7eb017dd06a8ea4e5604"),
    },
}
TX_FIELDS = [
    ("to", "address"), ("value", "uint256"), ("data", "bytes"), ("operation", "uint8"),
    ("safeTxGas", "uint256"), ("baseGas", "uint256"), ("gasPrice", "uint256"),
    ("gasToken", "address"), ("refundReceiver", "address"), ("nonce", "uint256"),
]


def address(value):
    if not re.fullmatch(r"0x[0-9a-fA-F]{40}", value or "") or value.lower() == ZERO:
        raise ValueError("expected a nonzero 20-byte address")
    return value.lower()


def hex_bytes(value, size=None):
    if not re.fullmatch(r"0x(?:[0-9a-fA-F]{2})*", value or ""):
        raise ValueError("expected 0x-prefixed bytes")
    if size is not None and len(value) != 2 + size * 2:
        raise ValueError(f"expected {size} bytes")
    return value.lower()


def uint256(value):
    if isinstance(value, bool) or not re.fullmatch(r"[0-9]+", str(value)) or not 0 <= int(value) < 2 ** 256:
        raise ValueError("expected an unsigned 256-bit integer")
    return int(value)


def cast(*args, allow_invalid_signature=False):
    # Cast failures can echo authenticated RPC URLs, so do not relay raw stderr.
    try:
        result = subprocess.run(["cast", *map(str, args)], capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired as error:
        raise ValueError(f"cast {args[0]} timed out after 60 seconds") from error
    if result.returncode:
        if allow_invalid_signature and result.returncode == 1 and result.stderr.startswith("Error: Validation failed."):
            return ""
        raise ValueError(f"cast {args[0]} failed; check inputs, contract ABI and RPC availability")
    return result.stdout.strip()


class Chain:
    def __init__(self, args):
        self.rpc = args.rpc_url
        if not self.rpc:
            raise ValueError("set L1_RPC_URL or --rpc-url")
        self.chain_id = int(cast("chain-id", "--rpc-url", self.rpc, "--rpc-timeout", 50))
        if self.chain_id != args.chain_id:
            raise ValueError(f"RPC chain ID {self.chain_id} differs from expected {args.chain_id}")
        self.safe = address(args.safe)
        self.factory = address(args.factory) if getattr(args, "factory", None) else None
        self.require_code(self.safe)
        self.owners = sorted(address(owner) for owner in self.call(self.safe, "getOwners()(address[])", json_result=True))
        if len(self.owners) != 2 or len(set(self.owners)) != 2 or int(self.call(self.safe, "getThreshold()(uint256)")) != 2:
            raise ValueError("expected a 2-of-2 Safe with two distinct owners")
        modules, _ = self.call(self.safe, "getModulesPaginated(address,uint256)(address[],address)", "0x" + "0" * 39 + "1", 1, json_result=True)
        if modules:
            raise ValueError("Safe modules can bypass the threshold; disable them first")

    def call(self, target, signature, *args, json_result=False):
        result = cast("call", target, signature, *args, "--rpc-url", self.rpc, "--rpc-timeout", 50, "--json")
        values = json.loads(result)
        if isinstance(values, dict):
            if values.get("success") is not True or not isinstance(values.get("data"), list):
                raise ValueError("unexpected cast call JSON response")
            values = values["data"]
        if not isinstance(values, list) or not values:
            raise ValueError("expected ABI outputs from cast call")
        # Cast JSON is an array of ABI outputs, including a nested array for address[].
        value = values[0] if len(values) == 1 else values
        if not json_result and isinstance(value, bool):
            return "true" if value else "false"
        return value if json_result else str(value)

    def require_code(self, target):
        if cast("code", address(target), "--rpc-url", self.rpc, "--rpc-timeout", 50) in ("0x", "0x0"):
            raise ValueError(f"no contract code at {target}")


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")
    print(f"Wrote {path}")


def builder(chain, target, data, name):
    chain.require_code(target)
    return {
        "version": "1.0", "chainId": str(chain.chain_id), "createdAt": int(time.time() * 1000),
        "meta": {"name": name, "description": "Review targets and calldata before signing.", "createdFromSafeAddress": chain.safe},
        "transactions": [{"to": address(target), "value": "0", "data": hex_bytes(data), "contractMethod": None, "contractInputsValues": None}],
    }


def council_data(chain, game):
    game = address(game)
    chain.require_code(game)
    if int(chain.call(game, "gameType()(uint32)")) != 1006:
        raise ValueError("game is not WIP-1006")
    factory = address(chain.call(game, "disputeGameFactory()(address)"))
    if chain.factory and factory != chain.factory:
        raise ValueError("game belongs to a different factory/network")
    chain.require_code(factory)
    claim = hex_bytes(chain.call(game, "rootClaim()(bytes32)"), 32)
    extra_data = hex_bytes(chain.call(game, "extraData()(bytes)"))
    registered, _ = chain.call(factory, "games(uint32,bytes32,bytes)(address,uint64)", 1006, claim, extra_data, json_result=True)
    if registered == ZERO or address(registered) != game:
        raise ValueError("game is not registered in its factory")
    if int(chain.call(game, "status()(uint8)")) != 0 or chain.call(game, "gameOver()(bool)") != "false":
        raise ValueError("game no longer accepts proofs")
    if int(chain.call(game, "proofBitmap()(uint8)")) & 4:
        raise ValueError("game already has a council proof")
    verifier = address(chain.call(game, "securityCouncil()(address)"))
    if address(chain.call(verifier, "council()(address)")) != chain.safe:
        raise ValueError("game council differs from GOVERNANCE_SAFE")
    root = hex_bytes(chain.call(game, "rootId()(bytes32)"), 32)
    digest = hex_bytes(chain.call(verifier, "attestationDigest(bytes32)(bytes32)", root), 32)
    expected_hash = hex_bytes(chain.call(chain.safe, "getMessageHash(bytes)(bytes32)", digest), 32)
    domain = cast("keccak", cast("abi-encode", "f(bytes32,uint256,address)",
        cast("keccak", "EIP712Domain(uint256 chainId,address verifyingContract)"), chain.chain_id, chain.safe))
    message = cast("keccak", cast("abi-encode", "f(bytes32,bytes32)",
        cast("keccak", "SafeMessage(bytes message)"), cast("keccak", digest)))
    actual_hash = hex_bytes(cast("keccak", "0x1901" + domain[2:] + message[2:]), 32)
    if actual_hash != expected_hash:
        raise ValueError("Safe message hash differs from the council attestation; incompatible fallback handler")
    return verifier, root, digest, expected_hash


def libraries(chain):
    version = chain.call(chain.safe, "VERSION()(string)")
    if chain.chain_id != 11155111 or version not in LIBRARIES:
        raise ValueError("council proposals require Sepolia and Safe 1.3.0, 1.4.1 or 1.5.0")
    targets = {}
    for name, (target, expected_hash) in LIBRARIES[version].items():
        code = hex_bytes(cast("code", target, "--rpc-url", chain.rpc, "--rpc-timeout", 50))
        if code == "0x" or cast("keccak", code).lower() != expected_hash:
            raise ValueError(f"canonical {name} library is missing or has unexpected bytecode")
        targets[name] = address(target)
    return version, targets


def multisend(actions):
    packed = "0x"
    for action in actions:
        data = hex_bytes(action["data"])[2:]
        packed += f'{action["operation"]:02x}' + address(action["to"])[2:]
        packed += f'{uint256(action["value"]):064x}{len(data) // 2:064x}' + data
    return cast("calldata", "multiSend(bytes)", packed)


def council_transaction(chain, game, recipient=None, nonce=None):
    verifier, root, digest, message_hash = council_data(chain, game)
    version, targets = libraries(chain)
    current_nonce = uint256(chain.call(chain.safe, "nonce()(uint256)"))
    nonce = current_nonce if nonce is None else uint256(nonce)
    if nonce < current_nonce:
        raise ValueError("Safe nonce was already executed; prepare a new transaction")
    recipient = address(recipient or chain.safe)
    actions = [
        {"to": targets["signMessage"], "operation": 1, "value": "0", "data": cast("calldata", "signMessage(bytes)", digest)},
        {"to": address(game), "operation": 0, "value": "0", "data": cast("calldata", "submitProofLane(bytes)", "0x02" + recipient[2:])},
    ]
    transaction = {
        "to": targets["multiSend"], "value": "0", "data": multisend(actions), "operation": 1,
        "safeTxGas": "0", "baseGas": "0", "gasPrice": "0", "gasToken": ZERO, "refundReceiver": ZERO, "nonce": nonce,
    }
    tx_hash = hex_bytes(chain.call(chain.safe,
        "getTransactionHash(address,uint256,bytes,uint8,uint256,uint256,uint256,address,address,uint256)(bytes32)",
        *(transaction[name] for name, _ in TX_FIELDS)), 32)
    # The handler executes the batch in Safe storage and reverts its state changes after simulation.
    chain.call(chain.safe, "simulate(address,bytes)(bytes)", transaction["to"], transaction["data"])
    return {
        "kind": "wip1006-council", "chainId": chain.chain_id, "safe": chain.safe, "safeVersion": version,
        "game": address(game), "councilVerifier": verifier, "rootId": root, "recipient": recipient,
        "attestationDigest": digest, "safeMessageHash": message_hash, "actions": actions,
        "safeTransaction": transaction, "safeTxHash": tx_hash,
    }


class NoRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


class Service:
    def __init__(self, url):
        parsed = urlsplit(url)
        if parsed.scheme != "https" or not parsed.netloc or parsed.username or parsed.password or parsed.query or parsed.fragment:
            raise ValueError("Safe service URL must use HTTPS without credentials, query or fragment")
        self.url = url.rstrip("/")
        self.opener = build_opener(NoRedirects())

    def request(self, method, path, body=None):
        headers = {"Content-Type": "application/json", "Accept": "application/json"}
        api_key = os.environ.get("SAFE_TRANSACTION_SERVICE_API_KEY")
        if api_key:
            if not re.fullmatch(r"[A-Za-z0-9._~-]+", api_key):
                raise ValueError("SAFE_TRANSACTION_SERVICE_API_KEY must be a bearer token without whitespace")
            headers["Authorization"] = "Bearer " + api_key
        request = Request(self.url + path, data=json.dumps(body).encode() if body is not None else None,
            headers=headers, method=method)
        try:
            with self.opener.open(request, timeout=20) as response:
                data = response.read(2 * 1024 * 1024 + 1)
            if len(data) > 2 * 1024 * 1024:
                raise ValueError("Safe service response exceeds 2 MiB")
            return json.loads(data) if data else None
        except HTTPError as error:
            hints = {
                400: "check transaction nonce, signature and Safe indexing",
                401: "set a valid SAFE_TRANSACTION_SERVICE_API_KEY",
                403: "check API permissions and Safe indexing",
                404: "check the service URL and wait for the Safe to be indexed",
                409: "check the pending transaction queue for a nonce conflict",
                429: "Safe API quota exceeded; wait or supply an API key",
            }
            raise ValueError(f"Safe service {method} failed (HTTP {error.code}); " + hints.get(error.code, "check service availability")) from error
        except (URLError, TimeoutError, OSError) as error:
            raise ValueError(f"Safe service {method} failed or timed out; check the Safe queue before retrying a proposal") from error
        except (UnicodeError, json.JSONDecodeError) as error:
            raise ValueError("Safe service returned malformed JSON; check the Safe queue before retrying a proposal") from error


def sign_transaction(args, chain, transaction):
    typed = {
        "types": {
            "EIP712Domain": [{"name": "chainId", "type": "uint256"}, {"name": "verifyingContract", "type": "address"}],
            "SafeTx": [{"name": name, "type": type_} for name, type_ in TX_FIELDS],
        },
        "primaryType": "SafeTx", "domain": {"chainId": chain.chain_id, "verifyingContract": chain.safe},
        "message": transaction,
    }
    wallet = ["--account", args.account] if args.account else ["--" + args.wallet]
    with tempfile.TemporaryDirectory() as directory:
        file = Path(directory) / "safe-transaction.json"
        file.write_text(json.dumps(typed))
        try:
            # Inherit stderr/stdin so Cast can show wallet prompts; no private key arguments.
            result = subprocess.run(["cast", "wallet", "sign", "--data", "--from-file", str(file), "--from", args.sender, *wallet],
                stdout=subprocess.PIPE, text=True, timeout=300)
        except subprocess.TimeoutExpired as error:
            raise ValueError("wallet signing timed out after 300 seconds; nothing was proposed") from error
    if result.returncode:
        raise ValueError("wallet signing failed; nothing was proposed")
    return hex_bytes(result.stdout.strip(), 65)


def describe(proposal):
    print(f'Safe: {proposal["safe"]}\nGame: {proposal["game"]}\nCouncil verifier: {proposal["councilVerifier"]}\n'
        f'Root ID: {proposal["rootId"]}\nReward recipient: {proposal["recipient"]}\n'
        f'Safe nonce: {proposal["safeTransaction"]["nonce"]}\nSafe transaction hash: {proposal["safeTxHash"]}\n'
        'Actions: approve council attestation, then submit the council lane (atomic)')


def propose(args, chain):
    proposal = json.loads(Path(args.transaction).read_text())
    if not isinstance(proposal, dict) or proposal.get("kind") != "wip1006-council":
        raise ValueError("expected a council-submit Safe transaction file; import CALL batches in Transaction Builder")
    expected = council_transaction(chain, proposal["game"], proposal["recipient"], proposal["safeTransaction"]["nonce"])
    if proposal != expected:
        raise ValueError("proposal differs from the current game, Safe, chain or canonical transaction; prepare it again")
    sender = address(args.sender)
    if sender not in chain.owners:
        raise ValueError("proposer must be a current Safe owner")
    safe = cast("to-check-sum-address", chain.safe)
    service = Service(args.service_url)
    nonce = proposal["safeTransaction"]["nonce"]
    pending = service.request("GET", f"/v2/safes/{safe}/multisig-transactions/?executed=false&nonce={nonce}")
    if not isinstance(pending, dict) or not isinstance(pending.get("results"), list) or pending.get("next"):
        raise ValueError("unexpected or incomplete Safe service pending-transaction response")
    for entry in pending["results"]:
        if not isinstance(entry, dict) or not isinstance(entry.get("safeTxHash"), str) or uint256(entry.get("nonce")) != nonce:
            raise ValueError("unexpected Safe service pending transaction")
        if hex_bytes(entry["safeTxHash"], 32) != proposal["safeTxHash"]:
            raise ValueError("another pending transaction uses this nonce; prepare again with --nonce <unused-nonce>")
    if pending["results"]:
        print(f'Transaction {proposal["safeTxHash"]} is already proposed; confirm it in the Safe UI.')
        return
    describe(proposal)
    signature = sign_transaction(args, chain, proposal["safeTransaction"])
    if signature[-2:] not in ("1b", "1c") or not cast("wallet", "verify", "--address", sender, "--no-hash",
            proposal["safeTxHash"], signature, allow_invalid_signature=True):
        raise ValueError("wallet signature does not match the proposer and Safe transaction; nothing was proposed")
    # Revalidate the game and simulation after the owner finishes reviewing in their wallet.
    current_chain = Chain(args)
    if sender not in current_chain.owners:
        raise ValueError("proposer is no longer a Safe owner; nothing was proposed")
    if council_transaction(current_chain, proposal["game"], proposal["recipient"], nonce) != proposal:
        raise ValueError("council transaction changed while signing; prepare it again")
    body = dict(proposal["safeTransaction"])
    for name, type_ in TX_FIELDS:
        if type_ == "address":
            body[name] = cast("to-check-sum-address", body[name])
    body.update(contractTransactionHash=proposal["safeTxHash"], sender=cast("to-check-sum-address", sender),
        signature=signature, origin="World Chain devnet council")
    service.request("POST", f"/v2/safes/{safe}/multisig-transactions/", body)
    print(f'Proposed {proposal["safeTxHash"]}; the other owner can confirm and execute in the Safe UI. Nothing was executed.')
    print(f"Safe queue: https://app.safe.global/transactions/queue?safe=sep:{safe}")


def prepare(args):
    chain = Chain(args)
    if args.operation == "call":
        data = args.data or cast("calldata", args.signature, *args.arguments)
        output = builder(chain, args.target, data, args.signature or "Administrative call")
        print(f"Target: {address(args.target)}\nFunction: {args.signature or 'raw calldata'}\nArguments: {args.arguments}")
    elif args.operation == "approve-pcrs":
        verifier = args.verifier
        if not verifier:
            verifier = json.loads((ROOT / f"pkg/contracts/deployments/{args.network}-nitro.json").read_text())["nitroAttestationVerifier"]
        if address(chain.call(address(verifier), "owner()(address)")) != chain.safe:
            raise ValueError("Safe does not own the Nitro attestation verifier")
        measurements = json.loads(Path(args.measurements).read_text())["nitro"]
        hashes = [cast("keccak", hex_bytes(measurements[f"pcr{i}"], 48)) for i in range(3)]
        output = builder(chain, verifier, cast("calldata", "approvePCRSet(bytes32,bytes32,bytes32)", *hashes), "Approve Nitro PCRs")
    elif args.operation == "council-submit":
        output = council_transaction(chain, args.game, args.recipient, args.nonce)
        describe(output)
    else:
        propose(args, chain)
        return
    write_json(args.out, output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--safe", default=os.environ.get("GOVERNANCE_SAFE"))
    parser.add_argument("--rpc-url", default=os.environ.get("L1_RPC_URL"))
    parser.add_argument("--chain-id", type=int, default=os.environ.get("L1_CHAIN_ID", "11155111"))
    parser.add_argument("--factory", default=os.environ.get("DISPUTE_GAME_FACTORY"), help="Expected factory for council operations")
    parser.add_argument("--out", help="Output file for preparation commands")
    sub = parser.add_subparsers(dest="operation", required=True)
    call = sub.add_parser("call", help="Prepare any zero-value contract CALL by ABI or raw calldata")
    call.add_argument("target")
    call.add_argument("signature", nargs="?")
    call.add_argument("arguments", nargs="*")
    call.add_argument("--data")
    pcr = sub.add_parser("approve-pcrs", help="Hash release measurements and prepare their approval")
    pcr.add_argument("--network", choices=("alphanet", "betanet"), default=os.environ.get("DEVNET_NETWORK", "alphanet"))
    pcr.add_argument("--verifier")
    pcr.add_argument("--measurements", default=str(ROOT / "proofs/measurements.json"))
    submit = sub.add_parser("council-submit", help="Prepare one atomic Safe transaction approving and submitting the council proof")
    submit.add_argument("--game", required=True)
    submit.add_argument("--recipient")
    submit.add_argument("--nonce", type=uint256, help="Safe nonce; defaults to the next onchain nonce")
    proposal = sub.add_parser("propose", help="Sign as one owner and publish the prepared council transaction to the Safe queue")
    proposal.add_argument("--transaction", required=True)
    proposal.add_argument("--sender", required=True, help="Proposing Safe owner address")
    proposal.add_argument("--service-url", default=os.environ.get("SAFE_TRANSACTION_SERVICE_URL", "https://api.safe.global/tx-service/sep/api"))
    wallet = proposal.add_mutually_exclusive_group(required=True)
    for option in ("browser", "interactive", "ledger", "trezor"):
        wallet.add_argument("--" + option, dest="wallet", action="store_const", const=option)
    wallet.add_argument("--account", help="Proposer's encrypted Foundry keystore")
    args = parser.parse_args()
    if (args.operation == "propose") == bool(args.out):
        parser.error("preparation requires --out; propose uses --transaction without --out")
    if args.operation == "call" and (bool(args.signature) == bool(args.data) or (args.data and args.arguments)):
        parser.error("provide either a function signature with arguments or --data")
    try:
        prepare(args)
    except (ValueError, OSError, KeyError, TypeError) as error:
        # File and parse errors contain public inputs; cast errors are redacted above.
        print(f"Error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
