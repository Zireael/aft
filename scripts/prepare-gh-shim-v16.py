#!/usr/bin/env python3
"""Prepare unsigned v16 from the unsigned v15 payload; never sign or install it.

v16 adds exactly one governed tuple, `pr create`, and the canonicalization
entry the shim needs to read it. Every other byte of v15 is kept: the script
refuses a baseline whose formatting would not survive a round trip, so no
pre-existing signed row is silently normalized.
"""
import argparse
import hashlib
import json
from pathlib import Path

PR_CREATE_ROW = {
    "tuple": "pr create",
    "platform": ["macos", "linux"],
    "reasoning": (
        "Speech tier, the same authority class as issue create: opening a pull "
        "request proposes a change under the bot identity and lands nothing. "
        "Merging stays on the admin pr merge row. The head branch must be in the "
        "target repository; cross-repository (owner:branch) heads are refused."
    ),
}
# The shim's `pr create` reader produces exactly these body fields in this
# order and refuses any other declaration, so the two must change together.
PR_CREATE_CANONICALIZATION = {
    "argv_forms": ["fields-only"],
    "target_fields": [],
    "body_fields": ["title", "body", "base", "head", "draft"],
}


def encode(value):
    return json.dumps(value, indent=2, ensure_ascii=False) + "\n"


def prepare(v15_path: Path, output: Path) -> bytes:
    raw = v15_path.read_bytes().decode("utf-8")
    manifest = json.loads(raw)
    if manifest["manifest_version"] != 15:
        raise ValueError("expected the reviewed unsigned v15 baseline")
    if encode(manifest) != raw:
        raise ValueError("baseline formatting differs; review before regenerating")
    for tier_rows in manifest["tiers"].values():
        if any(row["tuple"] == "pr create" for row in tier_rows):
            raise ValueError("baseline already declares pr create")
    if "pr create" in manifest["canonicalization"]:
        raise ValueError("baseline already canonicalizes pr create")
    manifest["manifest_version"] = 16
    manifest["tiers"]["governed"].append(PR_CREATE_ROW)
    manifest["canonicalization"]["pr create"] = PR_CREATE_CANONICALIZATION
    payload = encode(manifest).encode("utf-8")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(payload)
    print(f"{hashlib.sha256(payload).hexdigest()}  {output}")
    return payload


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("v15_unsigned", type=Path, help="unsigned v15 payload bytes")
    parser.add_argument(
        "output", type=Path, nargs="?",
        default=Path(".alfonso/ceremonies/gh-routing-manifest-v16/unsigned-v16.json"),
        help="unsigned output path (default: private .alfonso/ceremonies directory)",
    )
    args = parser.parse_args()
    prepare(args.v15_unsigned, args.output)
