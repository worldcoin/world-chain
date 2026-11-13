import argparse
from copy import deepcopy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError, URLError

spec = importlib.util.spec_from_file_location("safe_operation", Path(__file__).with_name("safe-operation.py"))
operation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(operation)

SAFE = "0x" + "11" * 20
GAME = "0x" + "22" * 20
VERIFIER = "0x" + "33" * 20
FACTORY = "0x" + "66" * 20
ROOT = "0x" + "44" * 32
DIGEST = "0x" + "55" * 32
SIGN_MESSAGE = operation.LIBRARIES["1.4.1"]["signMessage"][0].lower()
MULTISEND = operation.LIBRARIES["1.4.1"]["multiSend"][0].lower()


class FakeChain:
    safe = SAFE
    chain_id = 11155111
    factory = None
    registered_game = GAME
    rpc = "local-test"
    nonce = 7
    status = "0"
    game_over = "false"
    bitmap = "0"

    def __init__(self, owners):
        self.owners = sorted(owners)
        self.simulated = None

    def require_code(self, target):
        operation.address(target)

    def call(self, target, signature, *args, json_result=False):
        if signature == "gameType()(uint32)":
            return "1006"
        if signature == "securityCouncil()(address)":
            return VERIFIER
        if signature == "disputeGameFactory()(address)":
            return FACTORY
        if signature == "rootClaim()(bytes32)":
            return ROOT
        if signature == "extraData()(bytes)":
            return "0x"
        if signature == "games(uint32,bytes32,bytes)(address,uint64)":
            assert args == (1006, ROOT, "0x")
            return [self.registered_game, "1"]
        if signature == "status()(uint8)":
            return self.status
        if signature == "gameOver()(bool)":
            return self.game_over
        if signature == "proofBitmap()(uint8)":
            return self.bitmap
        if signature == "nonce()(uint256)":
            return str(self.nonce)
        if signature == "council()(address)":
            return SAFE
        if signature == "rootId()(bytes32)":
            return ROOT
        if signature == "attestationDigest(bytes32)(bytes32)":
            assert args == (ROOT,)
            return DIGEST
        if signature == "getMessageHash(bytes)(bytes32)":
            assert args == (DIGEST,)
            # Independently compute the Solidity handler's 0x1901/domain/struct hash.
            domain = operation.cast("keccak", operation.cast("abi-encode", "f(bytes32,uint256,address)",
                operation.cast("keccak", "EIP712Domain(uint256 chainId,address verifyingContract)"), self.chain_id, SAFE))
            message = operation.cast("keccak", operation.cast("abi-encode", "f(bytes32,bytes32)",
                operation.cast("keccak", "SafeMessage(bytes message)"), operation.cast("keccak", DIGEST)))
            return operation.cast("keccak", "0x1901" + domain[2:] + message[2:])
        if signature.startswith("getTransactionHash("):
            types = [type_ for _, type_ in operation.TX_FIELDS]
            type_hash = operation.cast("keccak", "SafeTx(" + ",".join(f"{type_} {name}" for name, type_ in operation.TX_FIELDS) + ")")
            hashed_args = list(args)
            hashed_args[2] = operation.cast("keccak", args[2])
            types[2] = "bytes32"
            struct_hash = operation.cast("keccak", operation.cast("abi-encode", "f(bytes32," + ",".join(types) + ")", type_hash, *hashed_args))
            domain = operation.cast("keccak", operation.cast("abi-encode", "f(bytes32,uint256,address)",
                operation.cast("keccak", "EIP712Domain(uint256 chainId,address verifyingContract)"), self.chain_id, SAFE))
            return operation.cast("keccak", "0x1901" + domain[2:] + struct_hash[2:])
        if signature == "simulate(address,bytes)(bytes)":
            assert target == SAFE and args[0] == MULTISEND
            self.simulated = args
            return "0x"
        raise AssertionError(signature)


class SafeOperationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.keys = ["0x" + "0" * 59 + "a11ce", "0x" + "0" * 61 + "b0b"]
        cls.owners = [operation.cast("wallet", "address", "--private-key", key).lower() for key in cls.keys]

    def test_rejects_malformed_addresses_and_hex(self):
        for value in (operation.ZERO, "0x1234", "$(command)", None):
            with self.assertRaises(ValueError):
                operation.address(value)
        for value in ("0x1", "0xzz", "12", "0x"):
            with self.assertRaises(ValueError):
                operation.hex_bytes(value, 48)
        for value in (True, -1, "0x1", 2 ** 256):
            with self.assertRaises(ValueError):
                operation.uint256(value)

    def test_cast_failure_redacts_rpc(self):
        failure = subprocess.CompletedProcess([], 1, "", "failed: https://rpc.example/credential")
        with patch.object(operation.subprocess, "run", return_value=failure):
            with self.assertRaisesRegex(ValueError, "cast call failed") as error:
                operation.cast("call", "--rpc-url", "https://rpc.example/credential")
        self.assertNotIn("credential", str(error.exception))

    def test_decodes_current_and_legacy_cast_json(self):
        chain = operation.Chain.__new__(operation.Chain)
        chain.rpc = "local-test"
        for response in ('["42"]', '{"schema_version":1,"success":true,"data":["42"]}'):
            with patch.object(operation, "cast", return_value=response):
                self.assertEqual(chain.call(SAFE, "getThreshold()(uint256)"), "42")
        for response in ('[true]', '{"schema_version":1,"success":true,"data":[true]}'):
            with patch.object(operation, "cast", return_value=response):
                self.assertEqual(chain.call(VERIFIER, "verify(bytes,bytes32,bytes)(bool)"), "true")

    def test_network_default_follows_devnet_wrapper(self):
        with patch.dict(operation.os.environ, {"DEVNET_NETWORK": "betanet"}), \
                patch.object(operation.sys, "argv", ["safe-operation", "--out", "pcr.json", "approve-pcrs"]), \
                patch.object(operation, "prepare") as prepare:
            self.assertEqual(operation.main(), 0)
            self.assertEqual(prepare.call_args[0][0].network, "betanet")

    def test_chain_rejects_wrong_threshold_and_modules(self):
        args = argparse.Namespace(rpc_url="local-test", chain_id=11155111, safe=SAFE)
        def fake_cast(*args):
            if args[0] == "chain-id":
                return "11155111"
            if args[0] == "code":
                return "0x1234"
            if args[2] == "getOwners()(address[])":
                return json.dumps([self.owners])
            if args[2] == "getThreshold()(uint256)":
                return '["1"]'
            raise AssertionError(args)
        with patch.object(operation, "cast", side_effect=fake_cast):
            with self.assertRaisesRegex(ValueError, "2-of-2"):
                operation.Chain(args)
        def module_cast(*args):
            if args[0] == "call" and args[2] == "getThreshold()(uint256)":
                return '["2"]'
            if args[0] == "call" and args[2].startswith("getModulesPaginated"):
                return json.dumps([[VERIFIER], operation.ZERO])
            return fake_cast(*args)
        with patch.object(operation, "cast", side_effect=module_cast):
            with self.assertRaisesRegex(ValueError, "modules"):
                operation.Chain(args)
        with patch.object(operation, "cast", return_value="1"):
            with self.assertRaisesRegex(ValueError, "chain ID"):
                operation.Chain(args)

    def test_builder_exports_call_fields(self):
        data = operation.cast("calldata", "setImplementation(uint32,address,bytes)", 1006, VERIFIER, "0x")
        output = operation.builder(FakeChain(self.owners), GAME, data, "Register game")
        self.assertEqual(output["chainId"], "11155111")
        self.assertEqual(output["meta"]["createdFromSafeAddress"], SAFE)
        self.assertEqual(output["transactions"][0]["value"], "0")
        self.assertEqual(output["transactions"][0]["data"], data)

    def proposal(self, chain):
        with patch.object(operation, "libraries", return_value=("1.4.1", {"signMessage": SIGN_MESSAGE, "multiSend": MULTISEND})):
            return operation.council_transaction(chain, GAME)

    def test_council_batch_approves_exact_message_and_submits_empty_proof(self):
        chain = FakeChain(self.owners)
        proposal = self.proposal(chain)
        transaction = proposal["safeTransaction"]
        self.assertEqual(transaction["operation"], 1)
        self.assertEqual(transaction["nonce"], 7)
        self.assertEqual(transaction["to"], MULTISEND)
        packed = operation.cast("abi-decode", "multiSend(bytes)", transaction["data"][10:], "--input")[2:]
        actions = []
        while packed:
            length = int(packed[106:170], 16)
            actions.append((int(packed[:2], 16), "0x" + packed[2:42], int(packed[42:106], 16), "0x" + packed[170:170 + length * 2]))
            packed = packed[170 + length * 2:]
        self.assertEqual(len(actions), 2)
        self.assertEqual(actions[0][:3], (1, SIGN_MESSAGE, 0))
        self.assertEqual(operation.cast("abi-decode", "signMessage(bytes)", actions[0][3][10:], "--input"), DIGEST)
        self.assertEqual(actions[1][:3], (0, GAME, 0))
        self.assertEqual(operation.cast("abi-decode", "submitProofLane(bytes)", actions[1][3][10:], "--input"), "0x02" + SAFE[2:])
        self.assertEqual(chain.simulated, (MULTISEND, transaction["data"]))

    def test_library_targets_require_supported_version_and_matching_code(self):
        chain = FakeChain(self.owners)
        with patch.object(chain, "call", return_value="1.2.0"):
            with self.assertRaisesRegex(ValueError, "Safe 1.3.0"):
                operation.libraries(chain)
        with patch.object(chain, "call", return_value="1.4.1"), patch.object(operation, "cast", return_value="0x1234"):
            with self.assertRaisesRegex(ValueError, "unexpected bytecode"):
                operation.libraries(chain)
        for version, entries in operation.LIBRARIES.items():
            def matching_code(*args):
                if args[0] == "code":
                    return args[1].lower()
                if args[0] == "keccak":
                    return next(hash_ for target, hash_ in entries.values() if target.lower() == args[1])
                raise AssertionError(args)
            with patch.object(chain, "call", return_value=version), patch.object(operation, "cast", side_effect=matching_code):
                self.assertEqual(operation.libraries(chain)[0], version)

    def test_council_rejects_closed_or_already_proven_game(self):
        for field, value, error in (("status", "1", "no longer"), ("game_over", "true", "no longer"), ("bitmap", "4", "already")):
            chain = FakeChain(self.owners)
            setattr(chain, field, value)
            with self.assertRaisesRegex(ValueError, error):
                operation.council_data(chain, GAME)

    def test_propose_requires_one_owner_transaction_signature(self):
        chain = FakeChain(self.owners)
        proposal = self.proposal(chain)
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            file = folder / "council.json"
            file.write_text(json.dumps(proposal))
            args = argparse.Namespace(transaction=str(file), sender=self.owners[0], account="test", wallet=None,
                service_url="https://api.safe.global/tx-service/sep/api")
            service = Mock()
            service.request.return_value = {"results": [], "next": None}
            original_run = operation.subprocess.run
            def sign_only_one_owner(command, **kwargs):
                if command[:3] != ["cast", "wallet", "sign"]:
                    return original_run(command, **kwargs)
                typed = json.loads(Path(command[command.index("--from-file") + 1]).read_text())
                self.assertEqual(typed["primaryType"], "SafeTx")
                self.assertEqual(typed["message"], proposal["safeTransaction"])
                # Only this proposer's key is used; no council-message signatures are collected.
                return original_run(["cast", "wallet", "sign", "--data", json.dumps(typed), "--private-key", self.keys[0]], **kwargs)
            with patch.object(operation, "council_transaction", return_value=proposal), \
                    patch.object(operation, "Chain", return_value=chain), \
                    patch.object(operation, "Service", return_value=service), \
                    patch.object(operation.subprocess, "run", side_effect=sign_only_one_owner):
                operation.propose(args, chain)
            self.assertEqual([call.args[0] for call in service.request.call_args_list], ["GET", "POST"])
            body = service.request.call_args_list[1].args[2]
            self.assertEqual(body["contractTransactionHash"], proposal["safeTxHash"])
            self.assertEqual(body["operation"], 1)
            self.assertEqual(body["sender"].lower(), self.owners[0])

    def test_propose_rejects_tampering_wrong_owner_and_invalid_signature(self):
        chain = FakeChain(self.owners)
        proposal = self.proposal(chain)
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / "council.json"
            args = argparse.Namespace(transaction=str(file), sender=self.owners[0], service_url="https://service.invalid/api")
            with patch.object(operation, "council_transaction", return_value=proposal), patch.object(operation, "sign_transaction") as sign:
                for field, value in (("safe", VERIFIER), ("chainId", 1), ("rootId", DIGEST)):
                    changed = deepcopy(proposal)
                    changed[field] = value
                    file.write_text(json.dumps(changed))
                    with self.assertRaisesRegex(ValueError, "differs"):
                        operation.propose(args, chain)
                file.write_text(json.dumps(proposal))
                args.sender = VERIFIER
                with self.assertRaisesRegex(ValueError, "current Safe owner"):
                    operation.propose(args, chain)
                sign.assert_not_called()
                args.sender = self.owners[0]
                sign.return_value = operation.cast("wallet", "sign", "--no-hash", proposal["safeTxHash"], "--private-key", self.keys[1])
                service = Mock()
                service.request.return_value = {"results": [], "next": None}
                with patch.object(operation, "Service", return_value=service):
                    with self.assertRaisesRegex(ValueError, "does not match"):
                        operation.propose(args, chain)
                self.assertEqual(service.request.call_count, 1)

    def test_propose_checks_pending_nonce_and_does_not_duplicate_proposals(self):
        chain = FakeChain(self.owners)
        proposal = self.proposal(chain)
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / "council.json"
            file.write_text(json.dumps(proposal))
            args = argparse.Namespace(transaction=str(file), sender=self.owners[0], service_url="https://service.invalid/api")
            service = Mock()
            service.request.return_value = {"results": [{"nonce": 7, "safeTxHash": DIGEST}], "next": None}
            with patch.object(operation, "council_transaction", return_value=proposal), \
                    patch.object(operation, "Service", return_value=service), patch.object(operation, "sign_transaction") as sign:
                with self.assertRaisesRegex(ValueError, "pending transaction uses this nonce"):
                    operation.propose(args, chain)
                service.request.return_value["results"][0]["safeTxHash"] = proposal["safeTxHash"]
                operation.propose(args, chain)
                sign.assert_not_called()
                self.assertEqual(service.request.call_count, 2)

    def test_council_nonce_rejects_already_executed_transaction(self):
        chain = FakeChain(self.owners)
        with patch.object(operation, "libraries", return_value=("1.4.1", {})):
            with self.assertRaisesRegex(ValueError, "already executed"):
                operation.council_transaction(chain, GAME, nonce=6)

    def test_propose_rechecks_nonce_after_wallet_signing(self):
        chain = FakeChain(self.owners)
        proposal = self.proposal(chain)
        signature = operation.cast("wallet", "sign", "--no-hash", proposal["safeTxHash"], "--private-key", self.keys[0])
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / "council.json"
            file.write_text(json.dumps(proposal))
            args = argparse.Namespace(transaction=str(file), sender=self.owners[0], service_url="https://service.invalid/api")
            def signing(*args):
                chain.nonce = 8
                return signature
            service = Mock()
            service.request.return_value = {"results": [], "next": None}
            with patch.object(operation, "libraries", return_value=("1.4.1", {"signMessage": SIGN_MESSAGE, "multiSend": MULTISEND})), \
                    patch.object(operation, "Chain", return_value=chain), patch.object(operation, "Service", return_value=service), \
                    patch.object(operation, "sign_transaction", side_effect=signing):
                with self.assertRaisesRegex(ValueError, "already executed"):
                    operation.propose(args, chain)
            self.assertEqual(service.request.call_count, 1)

    def test_wallet_timeout_stops_proposal(self):
        args = argparse.Namespace(account="test", sender=self.owners[0])
        with patch.object(operation.subprocess, "run", side_effect=subprocess.TimeoutExpired("cast", 300)):
            with self.assertRaisesRegex(ValueError, "nothing was proposed"):
                operation.sign_transaction(args, FakeChain(self.owners), {})

    def test_service_redacts_credentials_bounds_timeout_and_disallows_redirects(self):
        for url in ("http://service.invalid", "https://user:secret@service.invalid", "https://service.invalid?key=secret"):
            with self.assertRaises(ValueError):
                operation.Service(url)
        service = operation.Service("https://service.invalid/api")
        for failure, error in ((HTTPError("https://secret.invalid", 401, "secret", {}, None), "API_KEY"),
                (HTTPError("https://secret.invalid", 429, "secret", {}, None), "quota"),
                (URLError("credential"), "queue before retrying")):
            with patch.object(service.opener, "open", side_effect=failure) as request:
                with self.assertRaisesRegex(ValueError, error) as raised:
                    service.request("POST", "/v2/test/", {})
                self.assertNotIn("secret", str(raised.exception))
                self.assertNotIn("credential", str(raised.exception))
                self.assertEqual(request.call_args.kwargs["timeout"], 20)
        self.assertIsNone(operation.NoRedirects().redirect_request(None, None, 302, "", {}, "https://elsewhere.invalid"))

    def test_service_rejects_malformed_json_and_api_credentials(self):
        service = operation.Service("https://service.invalid/api")
        with patch.object(service.opener, "open") as request:
            request.return_value.__enter__.return_value.read.return_value = b"not JSON"
            with self.assertRaisesRegex(ValueError, "malformed JSON"):
                service.request("GET", "/v2/test/")
            request.reset_mock()
            with patch.dict(operation.os.environ, {"SAFE_TRANSACTION_SERVICE_API_KEY": "secret\r\nheader"}):
                with self.assertRaisesRegex(ValueError, "bearer token") as raised:
                    service.request("POST", "/v2/test/", {})
                self.assertNotIn("secret", str(raised.exception))
                request.assert_not_called()

    def test_council_rejects_wrong_network_and_unregistered_game(self):
        chain = FakeChain(self.owners)
        chain.factory = VERIFIER
        with self.assertRaisesRegex(ValueError, "different factory/network"):
            operation.council_data(chain, GAME)
        chain.factory = FACTORY
        chain.registered_game = SAFE
        with self.assertRaisesRegex(ValueError, "not registered"):
            operation.council_data(chain, GAME)


if __name__ == "__main__":
    unittest.main()
