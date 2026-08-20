#!/usr/bin/env python3
"""Validate proof-releases.lock.

Checks (always):
  - version keys are semver, latest_stable / latest_rc are "" or point at entries
  - measurement fields are present with the right shape
  - tee_image_id == keccak256(pcr0)

Modes:
  --base FILE       append-only check: every entry in FILE must exist unchanged
  --current         print the current entry (latest_rc, else latest_stable) as JSON
  --get VERSION     print one entry as JSON (fails if missing)
  --image-id PCR0   print keccak256(pcr0) — the tee_image_id — and exit
  --version VERSION cut a release: append a new entry and advance latest_rc
                    (--stable for latest_stable). Requires --vkeys and --pcrs,
                    both rebuilt from source. Use `just proof-release` rather
                    than calling this directly

Requires Python >= 3.11 and pycryptodome (for keccak256).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib

try:
    from Crypto.Hash import keccak as _keccak
except ImportError:
    print("missing dependency: run `pip install pycryptodome`", file=sys.stderr)
    sys.exit(1)


def keccak256(data: bytes) -> bytes:
    return _keccak.new(digest_bits=256, data=data).digest()

SEMVER_RE = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?"
    r"(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$"
)

# field name -> (hex length in nibbles, requires 0x prefix)
FIELDS = {
    "aggregation_vkey": (64, True),
    "range_vkey_commitment": (64, True),
    "aggregation_elf_sha256": (64, False),
    "range_elf_sha256": (64, False),
    "pcr0": (96, False),
    "pcr1": (96, False),
    "pcr2": (96, False),
    "tee_image_id": (64, True),
}

# --- validation --------------------------------------------------------------


def fail(msg: str) -> None:
    print(f"proof-releases.lock: {msg}", file=sys.stderr)
    sys.exit(1)


def load(path: str) -> dict:
    with open(path, "rb") as f:
        return tomllib.load(f)


def validate(doc: dict, path: str) -> dict:
    releases = doc.get("releases", {})
    if not isinstance(releases, dict):
        fail("`releases` must be a table")
    for pointer in ("latest_stable", "latest_rc"):
        if pointer not in doc:
            fail(f"missing `{pointer}`")
        val = doc[pointer]
        if val != "" and val not in releases:
            fail(f"`{pointer}` = {val!r} has no [releases.\"{val}\"] entry")
    for version, entry in releases.items():
        if not SEMVER_RE.match(version):
            fail(f"version key {version!r} is not semver")
        for field, (nibbles, prefixed) in FIELDS.items():
            if field not in entry:
                fail(f"[releases.\"{version}\"] missing `{field}`")
            val = entry[field]
            hexpart = val[2:] if prefixed else val
            if prefixed and not val.startswith("0x"):
                fail(f"[releases.\"{version}\"] `{field}` must be 0x-prefixed")
            if not prefixed and val.startswith("0x"):
                fail(f"[releases.\"{version}\"] `{field}` must not be 0x-prefixed")
            if len(hexpart) != nibbles or not re.fullmatch(r"[0-9a-f]+", hexpart):
                fail(f"[releases.\"{version}\"] `{field}` must be {nibbles} lowercase hex chars")
        derived = "0x" + keccak256(bytes.fromhex(entry["pcr0"])).hex()
        if derived != entry["tee_image_id"]:
            fail(
                f"[releases.\"{version}\"] tee_image_id {entry['tee_image_id']} "
                f"!= keccak256(pcr0) = {derived}"
            )
    return releases


def check_append_only(base_doc: dict, releases: dict) -> None:
    for version, base_entry in base_doc.get("releases", {}).items():
        if version not in releases:
            fail(f"entry {version!r} was removed; proof-releases.lock is append-only")
        if releases[version] != base_entry:
            fail(f"entry {version!r} was modified; released entries are immutable")


def cut(args: argparse.Namespace, releases: dict) -> None:
    version = args.version
    if not SEMVER_RE.match(version):
        fail(f"--version {version!r} is not semver")
    if version in releases:
        fail(f"--version: entry {version!r} already exists")
    if not args.vkeys or not args.pcrs:
        fail("--version requires --vkeys and --pcrs (use `just proof-release`)")

    with open(args.vkeys) as f:
        vkeys = json.load(f)
    with open(args.pcrs) as f:
        pcrs = {k.lower(): v for k, v in json.load(f).items()}

    entry = {
        "aggregation_vkey": vkeys["aggregation_vkey"],
        "range_vkey_commitment": vkeys["range_vkey_commitment"],
        "aggregation_elf_sha256": vkeys["elfs"]["world-chain-aggregation"]["sha256"],
        "range_elf_sha256": vkeys["elfs"]["world-chain-range-ethereum"]["sha256"],
        "pcr0": pcrs["pcr0"],
        "pcr1": pcrs["pcr1"],
        "pcr2": pcrs["pcr2"],
    }
    entry["tee_image_id"] = "0x" + keccak256(bytes.fromhex(entry["pcr0"])).hex()

    with open(args.file) as f:
        text = f.read()
    pointer = "latest_stable" if args.stable else "latest_rc"
    text, n = re.subn(
        rf'^{pointer} = "[^"]*"$', f'{pointer} = "{version}"', text, count=1, flags=re.M
    )
    if n != 1:
        fail(f"--version: could not find `{pointer}` line to update")
    block = f'\n[releases."{version}"]\n'
    block += "".join(f'{k} = "{v}"\n' for k, v in entry.items())
    text = text.rstrip("\n") + "\n" + block

    validate(tomllib.loads(text), args.file)  # never write an invalid registry
    with open(args.file, "w") as f:
        f.write(text)
    print(f"added [releases.\"{version}\"] and set {pointer}; merging will cut proofs/v{version}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("file", nargs="?", default="proof-releases.lock")
    parser.add_argument("--base", help="prior version of the file for the append-only check")
    parser.add_argument("--current", action="store_true", help="print current entry as JSON")
    parser.add_argument("--get", metavar="VERSION", help="print one entry as JSON")
    parser.add_argument("--image-id", metavar="PCR0", help="print keccak256(pcr0) and exit")
    parser.add_argument("--version", metavar="VERSION", help="cut a release entry (see docstring)")
    parser.add_argument("--pcrs", help="pcrs.json (from scripts/build-eif.sh) for --version")
    parser.add_argument("--vkeys", help="vkeys.json (from the vkeys subcommand) for --version")
    parser.add_argument("--stable", action="store_true", help="--version advances latest_stable")
    args = parser.parse_args()

    if args.image_id:
        print("0x" + keccak256(bytes.fromhex(args.image_id.removeprefix("0x"))).hex())
        return

    doc = load(args.file)
    releases = validate(doc, args.file)

    if args.version:
        cut(args, releases)
        return

    if args.base:
        check_append_only(load(args.base), releases)

    if args.get:
        if args.get not in releases:
            fail(f"no entry for version {args.get!r}")
        print(json.dumps({"version": args.get, **releases[args.get]}, indent=2))
        return

    if args.current:
        current = doc["latest_rc"] or doc["latest_stable"]
        print(json.dumps({"version": current, **releases[current]} if current else {}, indent=2))
        return

    print(f"proof-releases.lock: OK ({len(releases)} release(s))")


if __name__ == "__main__":
    main()
