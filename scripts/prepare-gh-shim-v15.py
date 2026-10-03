#!/usr/bin/env python3
"""Prepare unsigned v15 from the active v13 envelope; never sign or install it."""
import argparse
import hashlib
import json
from pathlib import Path


def prepare(envelope_path: Path, output: Path) -> None:
    envelope = json.loads(envelope_path.read_bytes())
    raw = envelope["manifest_bytes"]
    original = json.loads(raw)
    if original["manifest_version"] != 13:
        raise ValueError("expected the reviewed production v13 baseline")
    # Require the original formatting so all pre-existing rows remain byte-for-byte
    # unchanged, rather than silently normalizing signed policy text.
    def encode(value):
        return json.dumps(value, indent=2, ensure_ascii=False) + "\n"

    if encode(original) != raw:
        raise ValueError("baseline formatting differs; review before regenerating")
    manifest = json.loads(raw)
    manifest["manifest_version"] = 15
    platforms = ["macos", "linux"]
    for tier, tuples in [("governed", ["issue create", "issue edit"]),
                         ("admin", ["pr edit", "label create", "run cancel"])]:
        for tuple_name in tuples:
            if any(row["tuple"] == tuple_name for row in manifest["tiers"][tier]):
                raise ValueError(f"baseline already declares {tuple_name}")
            manifest["tiers"][tier].append({"tuple": tuple_name, "platform": platforms})
    manifest["canonicalization"]["issue create"] = {
        "argv_forms": ["fields-only"], "target_fields": [],
        "body_fields": ["title", "body", "labels"],
    }
    manifest["canonicalization"]["issue edit"] = {
        "argv_forms": ["target-and-fields"], "target_fields": ["number"],
        "body_fields": ["title", "body", "add_labels", "remove_labels", "add_assignees", "remove_assignees"],
    }
    manifest["api_rules"].append({
        "method": "PATCH", "path_glob": "/repos/*/*/issues/comments/*",
        "tier": "governed", "platform": platforms,
    })
    payload = encode(manifest).encode("utf-8")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(payload)
    print(f"{hashlib.sha256(payload).hexdigest()}  {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("v13_envelope", type=Path)
    parser.add_argument(
        "output", type=Path, nargs="?",
        default=Path(".alfonso/ceremonies/gh-routing-manifest-v15/unsigned-v15.json"),
        help="unsigned output path (default: private .alfonso/ceremonies directory)",
    )
    args = parser.parse_args()
    prepare(args.v13_envelope, args.output)
