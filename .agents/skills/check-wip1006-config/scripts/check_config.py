#!/usr/bin/env python3
"""Read-only active configuration inspection; shared RPC transport, local cast Keccak."""

import argparse
from functools import lru_cache
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from urllib.parse import urlsplit

HELPER = Path(__file__).resolve().parents[2] / "investigate-game/scripts/investigate_game.py"
spec = importlib.util.spec_from_file_location("game_rpc", HELPER)
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
UNKNOWN = "UNKNOWN"
ZERO = "0x" + "00" * 20
GAME_TYPE = 1006  # pkg/contracts/src/dispute/lib/GameTypes.sol
NETWORKS = {"mainnet": (1, "ETHEREUM_PROVIDER"), "sepolia": (11155111, "ETHEREUM_SEPOLIA_PROVIDER")}
IMPL = {
    **{k: v[1] for k, v in base.GETTERS.items() if k in (
        "gameType", "proposerBond", "challengerBond", "aggregationVKey", "rangeVKeyCommitment",
        "teeImageId", "validityProofVerifier", "teeVerifier", "securityCouncil",
        "disputeGameFactory", "anchorStateRegistry", "bondVault")},
    "domainHash": "bytes32", "rollupConfigHash": "bytes32", "blockInterval": "uint256",
    "challengePeriod": "uint64", "proofPeriod": "uint64", "PROOF_THRESHOLD": "uint8",
    "PROOF_LANE_COUNT": "uint8", "CHALLENGER_REWARD_BPS": "uint256",
    "protocolFeeRecipient": "address",
}


@lru_cache(maxsize=256)
def keccak(value):
    try:
        result = subprocess.run(["cast", "keccak", value], capture_output=True, text=True,
                                timeout=5, check=True).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        raise ValueError("Local cast keccak failed; install Foundry and check PATH") from None
    return base.decode(result, "bytes32")


def address(value):
    if not re.fullmatch(r"0x[0-9a-fA-F]{40}", value) or value.lower() == ZERO:
        raise argparse.ArgumentTypeError("Expected a nonzero 20-byte Ethereum address")
    return value.lower()


def decode(value, kind):
    if kind == "bool":
        number = base.decode(value, "uint8")
        if number not in (0, 1):
            raise ValueError("Invalid ABI boolean")
        return bool(number)
    if kind == "anchor":
        data = base.hexdata(value)
        if len(data) != 128:
            raise ValueError("Invalid anchor tuple")
        return (base.decode("0x" + data[:64], "bytes32"), int(data[64:], 16))
    return base.decode(value, kind)


class Inspector:
    def __init__(self, url, network, registry):
        self.url, self.network, self.registry = url, network, registry
        self.checks, self.sources, self.sections = [], {}, {}
        self.block = None
        self.pcr_cache = {}
        self.sections["WIP-1006"] = dict.fromkeys(["implementation", *IMPL], UNKNOWN)
        self.sections["TEE Attestation"] = {"NitroAttestationVerifier": UNKNOWN, "PCR sets": UNKNOWN}

    def check(self, status, message):
        self.checks.append((status, message))

    def rpc(self, calls):
        return base.rpc(self.url, calls)

    def start(self):
        chain, snapshot = self.rpc([("eth_chainId", []), ("eth_getBlockByNumber", ["finalized", False])])
        if base.quantity(chain) != NETWORKS[self.network][0]:
            raise ValueError("RPC chain ID does not match supplied network")
        finality = "finalized"
        if snapshot is None or isinstance(snapshot, base.CallError) and snapshot.args[0].startswith(
                ("RPC error code -32601", "RPC error code -32602")):
            snapshot, = self.rpc([("eth_getBlockByNumber", ["latest", False])])
            finality = "latest (finalized unavailable)"
            self.check("WARNING", "Finalized tag unavailable; using explicitly reported latest snapshot")
        base.require(snapshot)
        if not isinstance(snapshot, dict):
            raise ValueError("Snapshot unavailable")
        self.number = base.quantity(snapshot.get("number"))
        self.hash = base.decode(snapshot.get("hash"), "bytes32")
        self.timestamp = base.quantity(snapshot.get("timestamp"))
        self.block = {"blockHash": self.hash, "requireCanonical": True}
        self.snapshot = f"{self.number} | {self.hash} | {base.utc(self.timestamp)} | {finality}"

    def collect(self, target, fields, prefix, args=""):
        """fields maps getter signatures (or names) to ABI return types."""
        values = dict.fromkeys(fields, UNKNOWN)
        for name in fields:
            self.sources[f"{prefix}.{name}"] = f"{target}.{name if '(' in name else name + '()'}"
        if target in (UNKNOWN, ZERO):
            self.check("ERROR", f"{prefix}: dependency address unavailable or zero; fields UNKNOWN")
            return values
        calls = [("eth_call", [{"to": target, "data": keccak(
            name if "(" in name else name + "()")[:10] + args}, self.block]) for name in fields]
        try:
            results = self.rpc(calls)
        except ValueError as error:
            results = [error] * len(fields)
        for (name, kind), result in zip(fields.items(), results):
            try:
                values[name] = decode(result, kind)
            except ValueError as error:
                self.check("ERROR", f"{prefix}.{name}: UNKNOWN ({error})")
        return values

    def code(self, addresses):
        if ZERO in addresses:
            self.check("ERROR", "Required contract dependency is the zero address")
        targets = sorted(set(a for a in addresses if a not in (UNKNOWN, ZERO)))
        if not targets:
            return
        try:
            results = self.rpc([("eth_getCode", [a, self.block]) for a in targets])
        except ValueError as error:
            results = [error] * len(targets)
        for target, result in zip(targets, results):
            try:
                if not base.hexdata(result):
                    raise ValueError("no contract bytecode")
            except ValueError as error:
                self.check("ERROR", f"Contract {target}: {error}")

    def compare(self, label, actual, expected, severity="ERROR"):
        if UNKNOWN in (actual, expected):
            self.check("UNKNOWN", label)
        else:
            self.check("OK" if actual == expected else severity,
                       label + ("" if actual == expected else f": got {actual}, expected {expected}"))

    def pcrs(self, verifier):
        if verifier in self.pcr_cache:
            return self.pcr_cache[verifier]
        if verifier in (UNKNOWN, ZERO):
            return UNKNOWN
        # Events only enumerate candidates; approval always comes from snapshot state.
        topic = keccak("PCRSetApproved(bytes32,bytes32,bytes32)")
        pending, triples, requests = [(0, self.number)], set(), 0
        deadline = time.monotonic() + 180
        try:
            while pending:
                if requests >= 64 or time.monotonic() >= deadline:
                    raise ValueError("PCR event scan budget exhausted; history incomplete")
                lo, hi = pending.pop()
                requests += 1
                result, = self.rpc([("eth_getLogs", [{"address": verifier, "topics": [topic],
                                                    "fromBlock": hex(lo), "toBlock": hex(hi)}])])
                if isinstance(result, base.CallError):
                    if lo == hi:
                        raise result
                    mid = (lo + hi) // 2
                    pending.extend([(lo, mid), (mid + 1, hi)])
                    continue
                base.require(result)
                if not isinstance(result, list) or len(result) > 10000:
                    raise ValueError("Invalid or excessive PCR log response")
                for log in result:
                    if (not isinstance(log, dict) or log.get("address", "").lower() != verifier
                            or log.get("topics") != [topic] or log.get("removed") is not False
                            or not lo <= base.quantity(log.get("blockNumber")) <= hi):
                        raise ValueError("Invalid PCR event metadata")
                    data = base.hexdata(log.get("data"))
                    if len(data) != 192:
                        raise ValueError("Invalid PCR event ABI")
                    triples.add(tuple("0x" + data[i:i + 64] for i in range(0, 192, 64)))
                    if len(triples) > 256:
                        raise ValueError("PCR set enumeration exceeds 256-set budget")
            approved = []
            for triple in sorted(triples):
                value = self.collect(verifier, {"isPCRSetApproved(bytes32,bytes32,bytes32)": "bool"},
                                     "PCR approval", "".join(p[2:] for p in triple))
                approved.append((*triple, next(iter(value.values()))))
            self.pcr_cache[verifier] = approved
        except ValueError as error:
            self.check("ERROR", f"PCR approval: UNKNOWN ({error})")
            self.pcr_cache[verifier] = UNKNOWN
        return self.pcr_cache[verifier]

    def implementation(self, target, label, factory, system):
        values = {"implementation": target, **self.collect(target, IMPL, label)}
        self.sections["WIP-1006" if label == "WIP-1006" else "NEW implementation"] = values
        self.sources[f"{label}.implementation"] = "factory.gameImpls(1006)" if label == "WIP-1006" else "--implementation argument"
        self.compare(f"{label}: implementation game type", values["gameType"], GAME_TYPE)
        self.compare(f"{label}: factory relationship", values["disputeGameFactory"], factory)
        self.compare(f"{label}: registry relationship", values["anchorStateRegistry"], self.registry)
        for name, kind in IMPL.items():
            value = values[name]
            if value != UNKNOWN and (kind in ("address", "bytes32") or name in (
                    "proposerBond", "challengerBond", "blockInterval", "challengePeriod")) and int(str(value), 16 if isinstance(value, str) else 10) == 0:
                self.check("ERROR", f"{label}.{name} is zero")
        if all(values[k] != UNKNOWN for k in ("PROOF_THRESHOLD", "PROOF_LANE_COUNT", "proofPeriod", "challengePeriod")):
            valid = 2 <= values["PROOF_THRESHOLD"] <= values["PROOF_LANE_COUNT"] == 3 and values["proofPeriod"] > values["challengePeriod"] > 0
            self.check("OK" if valid else "ERROR", f"{label}: proof threshold and timing constraints")
        if values["proposerBond"] != UNKNOWN and values["CHALLENGER_REWARD_BPS"] != UNKNOWN:
            bps = values["CHALLENGER_REWARD_BPS"]
            self.check("OK" if 0 < bps <= 10000 and values["proposerBond"] <= (2**256 - 1) // bps else "ERROR",
                       f"{label}: reward basis points and proposer bond arithmetic")
        if all(values[k] != UNKNOWN for k in ("domainHash", "rollupConfigHash", "blockInterval")) and system["l2ChainId"] != UNKNOWN:
            encoded = f'{system["l2ChainId"]:064x}{1:064x}' + values["rollupConfigHash"][2:] + f'{values["blockInterval"]:064x}'
            self.compare(f"{label}: domainHash (LibProof v1 encoding)", values["domainHash"], keccak("0x" + encoded))

        def dependency(name, parent, fields):
            result = self.collect(parent, fields, label + "." + name)
            values.update({name + "." + k: v for k, v in result.items()})
            return result

        vault = dependency("vault", values["bondVault"], {"token": "address", "systemConfig": "address", "disputeGameFactory": "address", "delay": "uint256"})
        self.compare(f"{label}: vault factory", vault["disputeGameFactory"], factory)
        self.compare(f"{label}: vault SystemConfig", vault["systemConfig"], self.system_address)
        dependency("token", vault["token"], {"decimals": "uint8"})
        sp1 = dependency("validity", values["validityProofVerifier"], {"sp1Verifier": "address"})
        council = dependency("council", values["securityCouncil"], {"council": "address"})
        tee = dependency("tee", values["teeVerifier"], {"registry": "address"})
        registry = dependency("nitroRegistry", tee["registry"], {"verifier": "address", "owner": "address"})
        nitro = dependency("attestation", registry["verifier"], {"certManager": "address", "p384Verifier": "address", "owner": "address", "MAX_AGE": "uint256", "CLOCK_SKEW_TOLERANCE": "uint256", "MAX_CABUNDLE_LEN": "uint256"})
        cert = dependency("certManager", nitro["certManager"], {"p384Verifier": "address", "owner": "address", "revoker": "address", "ROOT_CA_CERT_HASH": "bytes32", "ROOT_CA_CERT_NOT_AFTER": "uint64"})
        if cert["ROOT_CA_CERT_HASH"] != UNKNOWN:
            root = self.collect(nitro["certManager"], {"revoked(bytes32)": "bool"}, label + ".certRoot", cert["ROOT_CA_CERT_HASH"][2:])
            values["certRoot.revoked"] = root["revoked(bytes32)"]
            self.compare(f"{label}: Nitro root certificate not revoked", values["certRoot.revoked"], False)
        else:
            values["certRoot.revoked"] = UNKNOWN
        if cert["ROOT_CA_CERT_NOT_AFTER"] != UNKNOWN:
            self.check("OK" if cert["ROOT_CA_CERT_NOT_AFTER"] > self.timestamp else "ERROR", f"{label}: Nitro root certificate expiry")
        self.compare(f"{label}: certificate and attestation P384 verifiers", cert["p384Verifier"], nitro["p384Verifier"])
        self.code([target, values["bondVault"], vault["token"], values["validityProofVerifier"], sp1["sp1Verifier"],
                   values["securityCouncil"], values["teeVerifier"], tee["registry"], registry["verifier"], nitro["certManager"], nitro["p384Verifier"]])
        if council["council"] == ZERO:
            self.check("ERROR", f"{label}: zero council authority")
        pcrs = self.pcrs(registry["verifier"])
        values["PCR sets"] = pcrs
        if label == "WIP-1006":
            self.sections.pop("TEE Attestation", None)
        image = values["teeImageId"]
        if image == UNKNOWN or pcrs == UNKNOWN:
            self.check("UNKNOWN", f"{label}: approval of PCR0 matching teeImageId")
        else:
            matching = [p for p in pcrs if p[0] == image]
            status = "OK" if any(p[3] is True for p in matching) else "UNKNOWN" if any(p[3] == UNKNOWN for p in matching) else "WARNING"
            self.check(status, f"{label}: approved PCR triple for teeImageId: " + ("yes" if status == "OK" else "UNKNOWN" if status == "UNKNOWN" else "none; new key registrations blocked for this image"))
        return values

    def run(self, candidate=None):
        self.start()
        registry = self.collect(self.registry, {"disputeGameFactory": "address", "systemConfig": "address", "superchainConfig": "address", "paused": "bool", "respectedGameType": "uint32", "retirementTimestamp": "uint64", "disputeGameFinalityDelaySeconds": "uint256", "anchorGame": "address", "getAnchorRoot": "anchor"}, "AnchorStateRegistry")
        self.sections["System State"] = registry
        factory = registry["disputeGameFactory"]
        if factory in (ZERO, UNKNOWN):
            raise ValueError("DisputeGameFactory cannot be discovered via AnchorStateRegistry.disputeGameFactory()")
        fields = self.collect(factory, {"gameImpls(uint32)": "address", "initBonds(uint32)": "uint256", "gameArgs(uint32)": "bytes"}, "factory", f"{GAME_TYPE:064x}")
        self.sections["Factory"] = {"address": factory, **fields}
        self.compare("Factory init bond is zero (ETH); proposer bond uses the ERC-20 vault", fields["initBonds(uint32)"], 0)
        self.compare("Factory implementation args empty (WIP-1006 CWIA layout)", fields["gameArgs(uint32)"], "0x")
        self.compare("Registry respects WIP-1006", registry["respectedGameType"], GAME_TYPE, "WARNING")
        if registry["retirementTimestamp"] != UNKNOWN and registry["retirementTimestamp"] >= self.timestamp:
            self.check("WARNING", "Retirement timestamp is at/after snapshot; games created through that timestamp are retired")
        self.system_address = registry["systemConfig"]
        system = self.collect(self.system_address, {"superchainConfig": "address", "paused": "bool", "l2ChainId": "uint256"}, "SystemConfig")
        self.sections["SystemConfig"] = {"address": self.system_address, **system}
        self.compare("SuperchainConfig relationship", system["superchainConfig"], registry["superchainConfig"])
        self.compare("Registry and SystemConfig pause agree", registry["paused"], system["paused"])
        pause = self.collect(registry["superchainConfig"], {"paused(address)": "bool"}, "SuperchainConfig", "0" * 64)
        self.sections["SuperchainConfig"] = {"address": registry["superchainConfig"], "paused(address(0))": pause["paused(address)"]}
        self.sections["SuperchainConfig"].update(self.collect(registry["superchainConfig"], {"guardian": "address", "pauseExpiry": "uint256"}, "SuperchainConfig"))
        for label, value in (("global", pause["paused(address)"]), ("registry (global or chain-specific)", registry["paused"])):
            self.compare(label + " pause inactive", value, False, "WARNING")
        if pause["paused(address)"] is True:
            self.compare("Global pause propagated to registry", registry["paused"], True)
        self.code([self.registry, factory, self.system_address, registry["superchainConfig"]])
        target = fields["gameImpls(uint32)"]
        if target in (ZERO, UNKNOWN):
            self.sections["WIP-1006"] = dict.fromkeys(["implementation", *IMPL], UNKNOWN)
            raise ValueError("WIP-1006 not registered or factory implementation unreadable")
        self.check("OK", "WIP-1006 registered in factory")
        self.sections["WIP-1006"] = self.implementation(target, "WIP-1006", factory, system)
        if candidate:
            self.sections["NEW implementation"] = self.implementation(candidate, "NEW", factory, system)
        self.check("UNKNOWN", "Proof artifacts/key correctness and end-to-end availability cannot be established from getters")
        self.check("WARNING", "Supplied registry is the trust root; canonical deployment and bytecode identity not authenticated")

    def finish(self):
        if self.block is None:
            return
        current, = self.rpc([("eth_getBlockByNumber", [hex(self.number), False])])
        base.require(current)
        if not isinstance(current, dict) or base.decode(current.get("hash"), "bytes32") != self.hash:
            self.sections.clear()
            self.sources.clear()
            self.checks.clear()
            raise ValueError("Snapshot reorganized; discarded report. Rerun inspection")

    def render(self):
        lines = ["WIP-1006 Active Configuration", f"Network: {self.network} | Chain ID: {NETWORKS[self.network][0]} (requested)",
                 f"Snapshot: {getattr(self, 'snapshot', UNKNOWN)}", f"AnchorStateRegistry: {self.registry}"]
        for section, fields in self.sections.items():
            if section == "NEW implementation":
                continue  # The comparison below includes every candidate field.
            lines.extend(["", section])
            for key, value in fields.items():
                if key == "PCR sets":
                    lines.append("TEE Attestation (PCRs are Keccak digests)")
                    if value == UNKNOWN:
                        lines.append("PCR0/PCR1/PCR2/approval: UNKNOWN")
                    elif not value:
                        lines.append("No PCR approval events found through snapshot")
                    else:
                        lines.extend(f"PCR0={p[0]} PCR1={p[1]} PCR2={p[2]} approved={p[3]}" for p in value)
                else:
                    suffix = " [raw ERC-20 units]" if key in ("proposerBond", "challengerBond") else " [wei]" if key == "initBonds(uint32)" else ""
                    lines.append(f"{key}: {value}{suffix}")
        if "NEW implementation" in self.sections:
            lines.extend(["", "OLD (active) vs NEW (candidate)"])
            old, new = self.sections["WIP-1006"], self.sections["NEW implementation"]
            for key in dict.fromkeys([*old, *new]):
                before, after = old.get(key, UNKNOWN), new.get(key, UNKNOWN)
                status = "UNKNOWN" if UNKNOWN in (before, after) else "unchanged" if before == after else "CHANGED"
                lines.append(f"{key} [{status}]: OLD={before} | NEW={after}")
        lines.extend(["", "Checks", *[f"[{status}] {message}" for status, message in self.checks], "", "Discovered via"])
        grouped = {}
        for name, source in self.sources.items():
            if name.endswith(".implementation"):
                lines.append(f"{name}: {source}")
            else:
                target, getter = source.split(".", 1)
                grouped.setdefault(target, set()).add(getter)
        lines.extend(f"{target}: " + ", ".join(sorted(getters)) for target, getters in grouped.items())
        lines.extend(["", "Active = factory implementation at snapshot, used for new games after activation; existing games are not updated by setImplementation().",
                      "PCR revocation blocks new registrations, not existing signers. Implementation pins PCR0 only; PCR1/2 are allowlisted triples."])
        return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("anchor_state_registry", type=address)
    parser.add_argument("network", choices=NETWORKS)
    parser.add_argument("l1_rpc", nargs="?")
    parser.add_argument("--implementation", type=address)
    args = parser.parse_args()
    url = args.l1_rpc or os.environ.get(NETWORKS[args.network][1])
    try:
        valid_url = url and urlsplit(url).scheme in ("http", "https") and urlsplit(url).hostname
    except ValueError:
        valid_url = False
    if not valid_url:
        print(f"[ERROR] No usable RPC: supply l1_rpc or set {NETWORKS[args.network][1]}")
        return 1
    inspector = Inspector(url, args.network, args.anchor_state_registry)
    try:
        inspector.run(args.implementation)
    except ValueError as error:
        inspector.check("ERROR", str(error))
    try:
        inspector.finish()
    except ValueError as error:
        inspector.check("ERROR", f"Snapshot verification failed: {error}")
    print(inspector.render())
    return int(any(status == "ERROR" for status, _ in inspector.checks))


if __name__ == "__main__":
    sys.exit(main())
