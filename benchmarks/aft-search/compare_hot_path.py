#!/usr/bin/env python3
"""Compare rendered rows, excluding only transport IDs and volatile host footers."""
import argparse
import json
import re
from pathlib import Path


def without_volatile_footers(text):
    patterns = (
        r"\n\n\[AFT E[^\n]*\]$",
        r"\n\n<system-reminder>\nThis is the \d+(?:st|nd|rd|th) identical call \(same command, same output\) in \d+s\.[^\n]*\n</system-reminder>$",
    )
    while True:
        original = text
        for pattern in patterns:
            text = re.sub(pattern, "", text)
        if text == original:
            return text


def ranked_response(row):
    response = row["response"]
    # The text field includes ordered results, snippets, classifications and
    # the final count/continuation message.
    result = {key: response.get(key) for key in ("success", "status", "complete", "text")}
    if isinstance(result["text"], str):
        result["text"] = without_volatile_footers(result["text"])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    args = parser.parse_args()
    before = json.loads(args.before.read_text())
    after = json.loads(args.after.read_text())
    assert before["complete"] and after["complete"], "incomplete measurement run"
    assert len(before["rows"]) == len(after["rows"]), "row count changed"
    differences = []
    for old, new in zip(before["rows"], after["rows"]):
        assert (old["corpus"], old["revision"], old["query"], old["repeat"]) == (new["corpus"], new["revision"], new["query"], new["repeat"]), "different corpus/query plan"
        assert isinstance(old["response"].get("text"), str) and old["response"]["text"], "missing rendered rows"
        if ranked_response(old) != ranked_response(new):
            differences.append(f"ranked rows changed: {old['case']} repeat={old['repeat']}")
        else:
            print(f"{old['case']}[{old['repeat']}]: rows unchanged; dev ms {old['latency_ms']:.3f} -> {new['latency_ms']:.3f}")
    if differences:
        raise AssertionError("; ".join(differences))


if __name__ == "__main__":
    main()
