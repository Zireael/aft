#!/usr/bin/env python3
"""Compare complete rendered search rows, ignoring only transport request IDs."""
import argparse
import json
from pathlib import Path


def ranked_response(row):
    response = row["response"]
    # The text field includes ordered results, snippets, classifications and
    # the final count/continuation message.
    return {key: response.get(key) for key in ("success", "status", "complete", "text")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    args = parser.parse_args()
    before = json.loads(args.before.read_text())
    after = json.loads(args.after.read_text())
    assert before["complete"] and after["complete"], "incomplete measurement run"
    assert len(before["rows"]) == len(after["rows"]), "row count changed"
    for old, new in zip(before["rows"], after["rows"]):
        assert (old["corpus"], old["revision"], old["query"], old["repeat"]) == (new["corpus"], new["revision"], new["query"], new["repeat"]), "different corpus/query plan"
        assert isinstance(old["response"].get("text"), str) and old["response"]["text"], "missing rendered rows"
        assert ranked_response(old) == ranked_response(new), f"ranked rows changed: {old['case']} repeat={old['repeat']}"
        print(f"{old['case']}[{old['repeat']}]: rows unchanged; dev ms {old['latency_ms']:.3f} -> {new['latency_ms']:.3f}")


if __name__ == "__main__":
    main()
