#!/usr/bin/env python3
"""Measure standalone dev-profile search latency with isolated per-corpus storage.

Work-count measurements belong in the accompanying Rust measurement tests; this
runner records actual standalone responses rather than inferring work from time.
"""
from __future__ import annotations

import argparse
import json
from contextlib import contextmanager
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from run import AftClient, binary_sha256

QUERIES = {
    "natural_language": "how does the search index discover candidate files and verify exact matches before ranking results",
    "identifier": "SearchIndex::new",
    "code_literal": '"SearchIndex::new"',
    "common_rare": "search repeated_page_marker",
    "regex": "search.*index",
    "anchored": "runtime WARN search 42 index",
}


@contextmanager
def isolated_corpus(source: Path, revision: str):
    """Avoid worktree read-only artifact borrowing and concurrent source edits."""
    probe = subprocess.run(["git", "-C", str(source), "rev-parse", "--show-toplevel"], capture_output=True, text=True)
    with tempfile.TemporaryDirectory(prefix="aft-hot-path-corpus-") as temporary:
        root = Path(temporary)
        commit = None
        if probe.returncode == 0 and Path(probe.stdout.strip()).resolve() == source:
            commit = subprocess.check_output(["git", "-C", str(source), "rev-parse", revision], text=True).strip()
            with tempfile.TemporaryFile() as archive:
                subprocess.run(["git", "-C", str(source), "archive", commit], stdout=archive, check=True)
                archive.seek(0)
                subprocess.run(["tar", "-xf", "-", "-C", str(root)], stdin=archive, check=True)
        else:
            shutil.copytree(source, root, dirs_exist_ok=True, ignore=shutil.ignore_patterns(".git"))
        yield root, commit


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, action="append", required=True)
    parser.add_argument("--revision", default="HEAD", help="Commit to snapshot for Git-root corpora; pin this across before/after runs.")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--case", action="append", choices=list(QUERIES))
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--semantic", action="store_true")
    parser.add_argument("--ready-timeout", type=float, default=1800)
    args = parser.parse_args()
    binary = args.binary.resolve()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    report = {"complete": False, "profile": "dev", "binary_sha256": binary_sha256(binary), "semantic": args.semantic, "rows": []}
    for corpus in args.corpus:
        corpus = corpus.resolve()
        with isolated_corpus(corpus, args.revision) as (snapshot, commit), tempfile.TemporaryDirectory(prefix="aft-hot-path-") as storage:
            client = AftClient(binary, snapshot, args.ready_timeout, Path(storage), args.semantic)
            try:
                configured = client.call("configure", {
                    "project_root": str(snapshot), "harness": "opencode", "storage_dir": storage,
                    "config": [{"tier": "user", "source": "<aft-hot-path>", "doc": json.dumps({
                        "indexes": {"trigram": True, "semantic": args.semantic, "callgraph": False},
                    })}],
                }, timeout_secs=300)
                if not configured.get("success"):
                    raise RuntimeError(f"configure failed: {configured}")
                client.wait_for_indexes()
                for label, query in QUERIES.items():
                    if args.case and label not in args.case:
                        continue
                    for repeat in range(args.repeats):
                        start = time.perf_counter()
                        response = client.call("tool_call", {"session_id": "aft-hot-path", "name": "search", "arguments": {"query": query, "topK": 50, "includeTests": False}}, timeout_secs=300)
                        elapsed = (time.perf_counter() - start) * 1000
                        if not response.get("success"):
                            raise RuntimeError(f"search failed: {response}")
                        plan = response.get("structuredContent", {}).get("plan", {})
                        if label in {"code_literal", "anchored"} and plan.get("shape") != {"code_literal": "code_literal", "anchored": "log_excerpt"}[label]:
                            raise RuntimeError(f"unexpected routed shape for {label}: {plan}")
                        report["rows"].append({"corpus": str(corpus), "snapshot_root": str(snapshot), "revision": commit, "case": label, "query": query, "repeat": repeat, "latency_ms": elapsed, "response": response})
            except Exception as error:
                report["failure"] = str(error)
                raise
            finally:
                client.close()
                args.out.write_text(json.dumps(report, indent=2) + "\n")
    report["complete"] = True
    args.out.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
