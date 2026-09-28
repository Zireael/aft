#!/usr/bin/env python3
"""Summarize measured work counts and require identical ranked lane digests."""
import argparse
import json
from pathlib import Path


def read_rows(path):
    rows = {}
    for line in path.read_text().splitlines():
        if "HOT_PATH {" not in line:
            continue
        row = json.loads(line.split("HOT_PATH ", 1)[1])
        rows.setdefault(row["case"], {}).update(row)
    if not rows:
        raise ValueError(f"no HOT_PATH measurement rows in {path}")
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    before, after = read_rows(args.before), read_rows(args.after)
    if before.keys() != after.keys():
        raise ValueError("measurement case sets differ")
    result = []
    for case in before:
        old, new = before[case], after[case]
        if case.startswith(("lexical/", "exact/", "anchored/")):
            if not old.get("ranked_blake3") or old["ranked_blake3"] != new.get("ranked_blake3"):
                raise ValueError(f"ranked lane bytes changed: {case}")
        result.append({"case": case, "before": old, "after": new})
    args.out.write_text(json.dumps(result, indent=2) + "\n")
    print(f"{len(result)} cases compared; ranked lane bytes unchanged")


if __name__ == "__main__":
    main()
