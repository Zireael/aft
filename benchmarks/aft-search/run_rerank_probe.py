#!/usr/bin/env python3
"""Latency, memory, failure and determinism probe for a configured reranker.

Report-only companion to the quality suites. It starts one standalone AFT on
a copy of the pinned AFT evidence tree (`.bench/repos/aft-evidence-<sha>`),
served by the same vector packs as the real-query and concept replays, and
runs a fixed list of prose queries (the real-query rows' prose and the
concept cases) in phases. AFT remembers the first outcome (a reranked order,
or a skip) for each result list and replays it for that list afterwards, so a
phase that must reach the backend uses queries no earlier phase sent.

Before that, a separate AFT on a 30-file project measures how long the
reranker takes from publishing the config to being installed.

- `cold`: the first search after the index is ready. AFT starts building the
  reranker when the config is published, so this is usually its first
  inference (slow on a fresh ONNX session), or a "backend not ready" skip.
- `warmup`: unscored burn-in searches (see `bench_rerank.warm_up`).
- `first_inference`: the next search after the warm-up.
- `warm`: one search per query; per-search latency and the result order.
- `repeat`: the same queries again in the same process (the remembered order
  is replayed, so this is also the replay latency).
- `paging`: topK 10 at offsets 0 and 10 against one topK 20 request.
- `stall`, `error`, `recovered`, `killed`: with `--proxy-upstream`, the
  loopback adapter in front of the backend holds requests past the deadline,
  answers 503, serves normally again, and then is killed so connections are
  refused. With `--failure-queries` and no proxy, the same queries run with
  nothing broken, which gives the order to compare against.

With `--rerank` omitted the same phases run with reranking off, which gives
the base order and the base latency to compare against. The process's resident
memory and CPU time are sampled every 100 ms, as is any `--extra-pid` (an
out-of-process backend such as llama-server).
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from collections import ChainMap
from pathlib import Path
from typing import Any, Mapping, Optional

import bench_rerank
from run_concept_recall import DEFAULT_QUERY_PACK, check_query_pack, load_fixtures, vector_server
from run_real_query import (
    DEFAULT_BINARY,
    FIXTURE_PROVIDER_MODEL,
    NdjsonClient,
    _normalize_results,
    assert_reference_platform,
    load_inputs,
    runtime_evidence_tree,
)
from search_quality_lib import sha256_file
from vector_pack import read_pack

HERE = Path(__file__).resolve().parent
TOP = 20
LATENCY_QUERIES = 40
FAILURE_QUERIES = 8


def cpu_seconds(text: str) -> float:
    """Parse `ps -o time=` output ([[dd-]hh:]mm:ss.ss)."""
    text = text.strip()
    days = 0
    if "-" in text:
        day, text = text.split("-", 1)
        days = int(day)
    parts = [float(part) for part in text.split(":")]
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + part
    return days * 86400 + seconds


def sample(pid: int) -> Optional[tuple[int, float]]:
    result = subprocess.run(["ps", "-o", "rss=,time=", "-p", str(pid)], capture_output=True, text=True, check=False)
    fields = result.stdout.split()
    if result.returncode or len(fields) < 2:
        return None
    return int(fields[0]), cpu_seconds(fields[1])


class Sampler:
    def __init__(self, pids: Mapping[str, int]):
        self.pids = dict(pids)
        self.samples: list[dict[str, Any]] = []
        self.marks: list[tuple[str, float]] = []
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        while not self._stop.is_set():
            row: dict[str, Any] = {"t": time.monotonic()}
            for name, pid in self.pids.items():
                value = sample(pid)
                if value is not None:
                    row[name] = {"rss_kb": value[0], "cpu_s": value[1]}
            self.samples.append(row)
            self._stop.wait(0.1)

    def start(self) -> "Sampler":
        self._thread.start()
        return self

    def mark(self, name: str) -> None:
        self.marks.append((name, time.monotonic()))

    def stop(self) -> None:
        self._stop.set()
        self._thread.join(timeout=5)

    def window(self, start: str, end: str, name: str) -> dict[str, Any]:
        """Peak RSS and CPU use of one process between two marks."""
        times = dict(self.marks)
        rows = [row for row in self.samples if times[start] <= row["t"] <= times[end] and name in row]
        if len(rows) < 2:
            return {}
        wall = rows[-1]["t"] - rows[0]["t"]
        cpu = rows[-1][name]["cpu_s"] - rows[0][name]["cpu_s"]
        return {
            "wall_s": round(wall, 3),
            "cpu_s": round(cpu, 3),
            "mean_cores": round(cpu / wall, 3) if wall > 0 else None,
            "rss_start_mb": round(rows[0][name]["rss_kb"] / 1024, 1),
            "rss_peak_mb": round(max(row[name]["rss_kb"] for row in rows) / 1024, 1),
            "rss_end_mb": round(rows[-1][name]["rss_kb"] / 1024, 1),
        }


def percentile(values: list[float], fraction: float) -> Optional[float]:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, round(fraction * (len(ordered) - 1))))
    return round(ordered[index], 3)


def queries_for(manifest: Mapping[str, Any], fixtures: list[Mapping[str, Any]]) -> list[dict[str, Any]]:
    seen: set[tuple[str, bool]] = set()
    queries: list[dict[str, Any]] = []
    for row in manifest["rows"]:
        if "excluded_reason" in row or row.get("semantic_state") == "building":
            continue
        key = (str(row["query"]), bool(row["include_tests"]))
        if key not in seen:
            seen.add(key)
            queries.append({"query": key[0], "includeTests": key[1], "source": str(row["episode_id"])})
    for fixture in fixtures:
        key = (str(fixture["query"]), False)
        if key not in seen:
            seen.add(key)
            queries.append({"query": key[0], "includeTests": False, "source": "concept"})
    return queries


def measure_build(binary: Path, timeout: float) -> dict[str, Any]:
    """Config-to-installed time and resident memory on a tiny project.

    The evidence tree's index build hides the reranker build, so this starts a
    separate AFT on 30 small files with semantic search off (nothing to embed,
    so the index is ready at once), publishes the config, and searches every
    50 ms until the response no longer carries "rerank skipped: backend not
    ready". With reranking off it still reports resident memory, which is the
    base the reranker's memory is measured against.
    """
    from run import AftClient

    with tempfile.TemporaryDirectory(prefix="aft-rerank-build-") as directory:
        root = Path(directory) / "project"
        root.mkdir()
        for index in range(30):
            (root / f"module_{index}.py").write_text(
                f"def handler_{index}(request):\n    '''Parse the request body and return a response.'''\n    return parse(request)\n"
            )
        client = AftClient(binary, root, timeout, storage_dir=Path(directory) / "storage", semantic_search=False)
        sampler = Sampler({"aft": client.proc.pid}).start()
        note: Optional[str] = None
        try:
            sampler.mark("start")
            started = time.monotonic()
            client.configure()
            client.wait_for_indexes(require_search=True)
            while time.monotonic() - started < timeout:
                response, _ = client.semantic_search("parse the request body and return a response", 5)
                note = bench_rerank.skip_note(response)
                if note != "backend not ready":
                    break
                time.sleep(0.05)
            installed = time.monotonic() - started
            sampler.mark("installed")
            time.sleep(1.0)
            sampler.mark("settled")
        finally:
            client.close()
            sampler.stop()
    return {
        "config_to_installed_s": round(installed, 3),
        "first_note_after_install": note,
        "window": sampler.window("start", "settled", "aft"),
    }


def run(args: argparse.Namespace) -> int:
    assert_reference_platform()
    if args.rerank:
        os.environ[bench_rerank.CONFIG_ENV] = args.rerank
    else:
        os.environ.pop(bench_rerank.CONFIG_ENV, None)
    if args.cache:
        os.environ[bench_rerank.CACHE_ENV] = args.cache
    # Keep anything AFT writes outside the runners' own storage directories
    # away from the live daemon's storage.
    os.environ["AFT_STORAGE_DIR"] = tempfile.mkdtemp(prefix="aft-rerank-probe-storage-")
    binary = Path(args.binary).resolve()
    manifest, tree, pack_path, pack = load_inputs(Path(args.manifest).resolve())
    fixtures = load_fixtures(HERE / "fixtures.json")
    query_pack = read_pack(Path(args.query_pack))
    check_query_pack(query_pack, pack, sha256_file(pack_path))
    vectors = ChainMap(query_pack["vectors"], pack["vectors"])
    queries = queries_for(manifest, fixtures)
    needed = 2 + LATENCY_QUERIES + (4 * FAILURE_QUERIES if args.proxy_upstream or args.failure_queries else 0)
    if len(queries) < needed:
        raise SystemExit(f"need {needed} distinct queries, have {len(queries)}")

    records: list[dict[str, Any]] = []
    build = measure_build(binary, args.ready_timeout)
    print(f"rerank_build:{json.dumps(build, sort_keys=True)}", flush=True)
    proxy: Optional[subprocess.Popen[bytes]] = None
    control = f"http://127.0.0.1:{args.proxy_port}/control"

    def set_mode(mode: str) -> None:
        body = json.dumps({"mode": mode, "stall_seconds": args.stall_seconds}).encode()
        request = urllib.request.Request(control, data=body, headers={"content-type": "application/json"})
        urllib.request.urlopen(request, timeout=5).read()

    with tempfile.TemporaryDirectory(prefix="aft-rerank-probe-") as run_dir, runtime_evidence_tree(tree) as project_root:
        runtime = Path(run_dir)
        if args.proxy_upstream:
            proxy = subprocess.Popen(
                [sys.executable, str(HERE / "rerank_score_proxy.py"), "--upstream", args.proxy_upstream, "--port", str(args.proxy_port)],
            )
            time.sleep(1.0)
        with vector_server(vectors, str(pack["embed_template_version"]), runtime / "embeddings.log") as server:
            client = NdjsonClient(binary, project_root, runtime / "storage", runtime / "aft.stderr")
            pids = {"aft": client.proc.pid, **({"backend": args.extra_pid} if args.extra_pid else {})}
            sampler = Sampler(pids).start()
            try:
                sampler.mark("start")
                client.configure(f"http://127.0.0.1:{server.server_port}", FIXTURE_PROVIDER_MODEL, args.ready_timeout)
                client.wait_ready(args.ready_timeout)
                sampler.mark("ready")

                def search(phase: str, item: Mapping[str, Any], top_k: int = TOP, offset: int = 0) -> dict[str, Any]:
                    arguments: dict[str, Any] = {"query": item["query"], "topK": top_k, "includeTests": item["includeTests"]}
                    if offset:
                        arguments["offset"] = offset
                    started = time.perf_counter()
                    response = client.search(arguments)
                    elapsed = (time.perf_counter() - started) * 1000.0
                    record = {
                        "phase": phase,
                        "query": item["query"],
                        "includeTests": item["includeTests"],
                        "topK": top_k,
                        "offset": offset,
                        "elapsed_ms": round(elapsed, 3),
                        "note": bench_rerank.skip_note(response),
                        "paths": [
                            f"{result['path']}:{result.get('line') or result.get('startLine') or result.get('start_line') or ''}"
                            for result in _normalize_results(response, project_root)
                        ],
                    }
                    records.append(record)
                    return record

                search("cold", queries[0])
                sampler.mark("cold_done")
                warm = bench_rerank.warm_up(
                    lambda query: client.search({"query": query, "topK": TOP, "includeTests": False}),
                    bench_rerank.burn_in_queries(),
                    args.ready_timeout,
                ) if args.rerank else None
                sampler.mark("backend_installed")
                search("first_inference", queries[1])
                sampler.mark("burst_start")
                latency_set = queries[2 : 2 + LATENCY_QUERIES]
                for item in latency_set:
                    search("warm", item)
                sampler.mark("burst_end")
                for item in latency_set:
                    search("repeat", item)
                sampler.mark("repeat_end")
                for item in latency_set[:10]:
                    search("paging_full", item, 20, 0)
                    search("paging_page1", item, 10, 0)
                    search("paging_page2", item, 10, 10)
                if args.proxy_upstream or args.failure_queries:
                    rest = queries[2 + LATENCY_QUERIES :]
                    phases = [("stall", "stall"), ("error", "error"), ("recovered", "ok"), ("killed", None)]
                    for index, (phase, mode) in enumerate(phases):
                        if not args.proxy_upstream:
                            pass
                        elif mode is None:
                            assert proxy is not None
                            proxy.kill()
                            proxy.wait(timeout=5)
                            proxy = None
                        else:
                            set_mode(mode)
                        for item in rest[index * FAILURE_QUERIES : (index + 1) * FAILURE_QUERIES]:
                            search(phase, item)
                sampler.mark("end")
                stderr = client.stderr_text()
            finally:
                client.close()
                sampler.stop()
                if proxy is not None:
                    proxy.kill()
            refused = list(server.refused)

    warm_latency = [record["elapsed_ms"] for record in records if record["phase"] == "warm"]
    repeat_latency = [record["elapsed_ms"] for record in records if record["phase"] == "repeat"]
    windows = {
        name: sampler.window(start, end, "aft")
        for name, (start, end) in {
            "startup_to_ready": ("start", "ready"),
            "backend_build": ("cold_done", "backend_installed"),
            "query_burst": ("burst_start", "burst_end"),
        }.items()
    }
    if args.extra_pid:
        windows["backend_query_burst"] = sampler.window("burst_start", "burst_end", "backend")
    marks = dict(sampler.marks)
    result = {
        "label": args.label,
        "rerank": json.loads(args.rerank) if args.rerank else None,
        "binary_sha256": sha256_file(binary),
        "warmup": warm,
        "build": build,
        "backend_build_wall_s": round(marks["backend_installed"] - marks["cold_done"], 3),
        "latency_ms": {
            "cold_first_search": next(record["elapsed_ms"] for record in records if record["phase"] == "cold"),
            "first_inference": next(record["elapsed_ms"] for record in records if record["phase"] == "first_inference"),
            "warm_p50": percentile(warm_latency, 0.5),
            "warm_p95": percentile(warm_latency, 0.95),
            "warm_max": max(warm_latency) if warm_latency else None,
            "repeat_p50": percentile(repeat_latency, 0.5),
            "repeat_p95": percentile(repeat_latency, 0.95),
        },
        "notes": {phase: [record["note"] for record in records if record["phase"] == phase] for phase in dict.fromkeys(record["phase"] for record in records)},
        "resources": windows,
        "vector_refusals": refused,
        "stderr_tail": stderr[-2000:],
        "records": records,
    }
    output = Path(args.out)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps({key: result[key] for key in ("label", "backend_build_wall_s", "latency_ms", "resources")}, indent=2))
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    result.add_argument("--binary", default=DEFAULT_BINARY)
    result.add_argument("--manifest", default=str(HERE / "real-query-manifest.json"))
    result.add_argument("--query-pack", default=str(DEFAULT_QUERY_PACK))
    result.add_argument("--label", required=True)
    result.add_argument("--rerank", help="JSON search.rerank block; omit for reranking off")
    result.add_argument("--cache", help="model cache holding the ONNX reranker (see bench_rerank)")
    result.add_argument("--proxy-upstream", help="llama-server base URL; starts rerank_score_proxy.py and runs the failure phases")
    result.add_argument("--proxy-port", type=int, default=8092)
    result.add_argument("--failure-queries", action="store_true", help="run the failure phases' queries without breaking anything (the base order to compare a failure run against)")
    result.add_argument("--stall-seconds", type=float, default=30.0)
    result.add_argument("--extra-pid", type=int, help="also sample this process (an out-of-process backend)")
    result.add_argument("--ready-timeout", type=float, default=900.0)
    result.add_argument("--out", required=True)
    return result


if __name__ == "__main__":
    raise SystemExit(run(parser().parse_args()))
