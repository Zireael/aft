#!/usr/bin/env python3
"""Measure standalone dev-profile search latency with isolated per-corpus storage.

Work-count measurements belong in the accompanying Rust measurement tests; this
runner records actual standalone responses rather than inferring work from time.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import tempfile
import time

from run import AftClient, binary_sha256

QUERIES = {
    "natural_language": "how does the search index discover candidate files and verify exact matches before ranking results",
    "code_literal": "SearchIndex::new",
    "common_rare": "search repeated_page_marker",
    "anchored": "search.*index",
}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, action="append", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--semantic", action="store_true")
    parser.add_argument("--ready-timeout", type=float, default=1800)
    args = parser.parse_args()
    binary = args.binary.resolve()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    report = {"complete": False, "profile": "dev", "binary_sha256": binary_sha256(binary), "semantic": args.semantic, "rows": []}
    for corpus in args.corpus:
        corpus = corpus.resolve()
        with tempfile.TemporaryDirectory(prefix="aft-hot-path-") as storage:
            client = AftClient(binary, corpus, args.ready_timeout, Path(storage), args.semantic)
            try:
                client.configure()
                client.wait_for_indexes()
                for label, query in QUERIES.items():
                    for repeat in range(args.repeats):
                        start = time.perf_counter()
                        response = client.call("tool_call", {"session_id": "aft-hot-path", "name": "search", "arguments": {"query": query, "topK": 50, "includeTests": False}}, timeout_secs=300)
                        elapsed = (time.perf_counter() - start) * 1000
                        if not response.get("success"):
                            raise RuntimeError(f"search failed: {response}")
                        report["rows"].append({"corpus": str(corpus), "case": label, "query": query, "repeat": repeat, "latency_ms": elapsed, "response": response})
            finally:
                client.close()
                args.out.write_text(json.dumps(report, indent=2) + "\n")
    report["complete"] = True
    args.out.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
