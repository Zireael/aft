#!/usr/bin/env python3
"""FTS5 search quality benchmark.

Runs FTS5-specific fixture queries through AFT search and measures:
- Retrieval quality (Recall@k, MRR)
- Token/char efficiency (tokens returned per query)
- Latency (p50, p95)
- Exact-first invariant (exact symbol lookups rank above fuzzy matches)

Usage:
    python run_fts5_bench.py --binary ../../target/release/aft --project-root ../..
    python run_fts5_bench.py --binary ../../target/release/aft --project-root ../.. --out results/fts5-quality.json
"""

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path


def start_aft(binary: str, project_root: str, storage_dir: str) -> subprocess.Popen:
    """Start an AFT process with NDJSON protocol."""
    proc = subprocess.Popen(
        [binary],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=project_root,
        env={**os.environ, "RUST_LOG": "warn"},
        text=True,
        bufsize=1,
    )
    return proc


def send_command(proc: subprocess.Popen, command: dict) -> dict:
    """Send a command and receive the response."""
    line = json.dumps(command) + "\n"
    proc.stdin.write(line)
    proc.stdin.flush()

    # Read response line
    while True:
        resp_line = proc.stdout.readline()
        if not resp_line:
            raise RuntimeError("AFT process closed unexpectedly")
        resp = json.loads(resp_line.strip())
        # Skip push frames (have 'type' but no 'id')
        if "id" in resp:
            return resp


def configure_aft(proc: subprocess.Popen, project_root: str, storage_dir: str) -> dict:
    """Configure AFT with search enabled."""
    return send_command(proc, {
        "id": "cfg-bench",
        "command": "configure",
        "harness": "pi",
        "project_root": project_root,
        "storage_dir": storage_dir,
        "search_index": True,
        "semantic_search": True,
    })


def wait_for_index(proc: subprocess.Popen, timeout: float = 120.0) -> bool:
    """Wait for search index to be ready."""
    start = time.time()
    while time.time() - start < timeout:
        resp = send_command(proc, {
            "id": "status-check",
            "command": "status",
        })
        if resp.get("success") and resp.get("search_ready"):
            return True
        time.sleep(2.0)
    return False


def run_fts5_bench(binary: str, project_root: str, fixtures_path: str, out_path: str):
    """Run the FTS5 benchmark."""
    # Load fixtures
    with open(fixtures_path) as f:
        fixtures = json.load(f)

    # Create temporary storage
    storage_dir = os.path.join(project_root, ".bench", "fts5-storage")
    os.makedirs(storage_dir, exist_ok=True)

    # Start AFT
    print(f"Starting AFT from {binary}...")
    proc = start_aft(binary, project_root, storage_dir)

    try:
        # Configure
        print("Configuring AFT with search enabled...")
        cfg_resp = configure_aft(proc, project_root, storage_dir)
        if not cfg_resp.get("success"):
            print(f"Configure failed: {cfg_resp}")
            return

        # Wait for index
        print("Waiting for search index...")
        if not wait_for_index(proc):
            print("Warning: Index not ready within timeout, proceeding anyway")

        # Run queries
        results = []
        for i, fixture in enumerate(fixtures):
            query = fixture["query"]
            print(f"  [{i+1}/{len(fixtures)}] Query: {query}")

            start_time = time.time()
            resp = send_command(proc, {
                "id": f"search-{i}",
                "command": "aft_search",
                "query": query,
                "top_k": 10,
            })
            latency_ms = (time.time() - start_time) * 1000

            if not resp.get("success"):
                print(f"    Search failed: {resp.get('message', 'unknown error')}")
                results.append({
                    "query": query,
                    "shape": fixture.get("shape", "unknown"),
                    "intent": fixture.get("intent", "unknown"),
                    "error": resp.get("message", "unknown error"),
                    "latency_ms": latency_ms,
                })
                continue

            # Extract results
            search_results = resp.get("results", [])
            result_files = [r.get("file_path", "") for r in search_results]
            result_names = [r.get("symbol_name", "") for r in search_results]

            # Compute metrics
            expected_files = fixture.get("expected_top_files", [])
            expected_symbol = fixture.get("expected_symbol_name")

            # Recall@k
            recall_at_1 = 1.0 if result_files and result_files[0] in expected_files else 0.0
            recall_at_5 = sum(1 for f in result_files[:5] if f in expected_files) / max(len(expected_files), 1)
            recall_at_10 = sum(1 for f in result_files[:10] if f in expected_files) / max(len(expected_files), 1)

            # MRR (reciprocal rank of first relevant result)
            mrr = 0.0
            for rank, f in enumerate(result_files, 1):
                if f in expected_files:
                    mrr = 1.0 / rank
                    break

            # Exact-first invariant check
            exact_first = False
            if expected_symbol and result_names:
                exact_first = result_names[0] == expected_symbol

            # Token efficiency
            total_chars = sum(len(r.get("snippet", "")) for r in search_results)
            total_tokens_approx = total_chars // 4  # Rough estimate: 4 chars per token

            result = {
                "query": query,
                "shape": fixture.get("shape", "unknown"),
                "intent": fixture.get("intent", "unknown"),
                "expected_top_files": expected_files,
                "expected_symbol_name": expected_symbol,
                "result_files": result_files[:10],
                "result_symbols": result_names[:10],
                "retrieval_metrics": {
                    "recall_at_1": recall_at_1,
                    "recall_at_5": recall_at_5,
                    "recall_at_10": recall_at_10,
                    "mrr": mrr,
                },
                "exact_first": exact_first,
                "token_efficiency": {
                    "total_chars": total_chars,
                    "estimated_tokens": total_tokens_approx,
                    "results_returned": len(search_results),
                },
                "latency_ms": latency_ms,
            }
            results.append(result)
            print(f"    Recall@1={recall_at_1:.2f} MRR={mrr:.2f} exact_first={exact_first} latency={latency_ms:.0f}ms")

        # Compute aggregate metrics
        valid_results = [r for r in results if "error" not in r]
        if valid_results:
            agg_recall_1 = sum(r["retrieval_metrics"]["recall_at_1"] for r in valid_results) / len(valid_results)
            agg_recall_5 = sum(r["retrieval_metrics"]["recall_at_5"] for r in valid_results) / len(valid_results)
            agg_mrr = sum(r["retrieval_metrics"]["mrr"] for r in valid_results) / len(valid_results)
            agg_exact_first = sum(1 for r in valid_results if r["exact_first"]) / len(valid_results)
            latencies = [r["latency_ms"] for r in valid_results]
            latencies.sort()
            p50 = latencies[len(latencies) // 2] if latencies else 0
            p95 = latencies[int(len(latencies) * 0.95)] if latencies else 0
        else:
            agg_recall_1 = agg_recall_5 = agg_mrr = agg_exact_first = 0.0
            p50 = p95 = 0.0

        # Write output
        output = {
            "tool_name": "aft_fts5_search",
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "binary": binary,
            "project_root": project_root,
            "fixture_count": len(fixtures),
            "success_count": len(valid_results),
            "error_count": len(results) - len(valid_results),
            "aggregate": {
                "recall_at_1": agg_recall_1,
                "recall_at_5": agg_recall_5,
                "mrr": agg_mrr,
                "exact_first_rate": agg_exact_first,
                "latency_p50_ms": p50,
                "latency_p95_ms": p95,
            },
            "per_query": results,
        }

        os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
        with open(out_path, "w") as f:
            json.dump(output, f, indent=2)

        print(f"\n=== FTS5 Benchmark Results ===")
        print(f"Queries: {len(fixtures)} ({len(valid_results)} successful)")
        print(f"Recall@1: {agg_recall_1:.3f}")
        print(f"Recall@5: {agg_recall_5:.3f}")
        print(f"MRR: {agg_mrr:.3f}")
        print(f"Exact-first rate: {agg_exact_first:.3f}")
        print(f"Latency p50: {p50:.0f}ms, p95: {p95:.0f}ms")
        print(f"\nResults written to {out_path}")

    finally:
        # Shutdown AFT
        try:
            send_command(proc, {"id": "shutdown", "command": "shutdown"})
        except Exception:
            pass
        proc.terminate()
        proc.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description="FTS5 search quality benchmark")
    parser.add_argument("--binary", default="../../target/release/aft", help="Path to AFT binary")
    parser.add_argument("--project-root", default="../..", help="Path to project root")
    parser.add_argument("--fixtures", default=None, help="Path to fixtures JSON (default: fts5-fixtures.json)")
    parser.add_argument("--out", default="results/fts5-quality.json", help="Output path")
    args = parser.parse_args()

    fixtures_path = args.fixtures or os.path.join(os.path.dirname(__file__), "fts5-fixtures.json")
    run_fts5_bench(args.binary, args.project_root, fixtures_path, args.out)


if __name__ == "__main__":
    main()
