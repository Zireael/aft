"""Opt-in reranker configuration for the search benchmark runners (report-only).

The runners measure `aft_search` with `search.rerank` off, as the product ships.
Setting the environment variables below makes the exact-recall, concept-recall
and real-query runners configure a reranker instead, so the same suites can
compare backends. Nothing here changes a run when the variables are unset.

- `AFT_SEARCH_BENCH_RERANK`: a JSON object written verbatim as the user-tier
  `search.rerank` block, for example `{"backend": "onnx", "model":
  "gte-reranker-modernbert-base", "timeout_ms": 15000}`.
- `AFT_SEARCH_BENCH_RERANK_CACHE`: a directory used as `FASTEMBED_CACHE_DIR` for
  the AFT process. The ONNX reranker reads its pinned model from that cache. The
  real-query and concept runners block downloads, so the model must already be
  there. It replaces the runner's own (empty) model cache, so point it at a
  directory that holds only the reranker model: the pack replays must embed
  only through the vector pack, and an embedding model found in the cache
  would give AFT a local model to embed with instead.
- `AFT_SEARCH_BENCH_RERANK_LOG`: a file that every search request is appended
  to as one JSON line (runner, query, elapsed milliseconds, and the reranker's
  skip note when the response carried one). The scores keep their checked-in
  shape and say nothing about the reranker, so this log is where a reader sees
  whether each search was reranked or skipped (the engine reports a skip only
  as text appended to the response).
- `AFT_SEARCH_BENCH_SETTLE_SECONDS`: see `settle`. Not reranker-specific; it
  lets a macOS run compare two configurations without the watcher's
  re-embedding burst landing on different rows in each.

AFT builds the reranker on a background thread when the configuration is
published, and until it is installed every search keeps fused order with the
note "rerank skipped: backend not ready". `warm_up` therefore waits for it, and
lets it score a few queries no suite scores, before the scored rows run, so no
scored row is measured against a backend that is still loading.
"""
from __future__ import annotations

import json
import os
import re
import time
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, MutableMapping, Optional

CONFIG_ENV = "AFT_SEARCH_BENCH_RERANK"
CACHE_ENV = "AFT_SEARCH_BENCH_RERANK_CACHE"
LOG_ENV = "AFT_SEARCH_BENCH_RERANK_LOG"
SETTLE_ENV = "AFT_SEARCH_BENCH_SETTLE_SECONDS"

# The engine appends "(rerank skipped: <reason>)" to the response text when a
# configured reranker did not reorder the list.
_NOTE = re.compile(r"\(rerank skipped: ([^)\n]*)\)")
_NOT_READY = "backend not ready"
# Whether searches logged right now are warm-up searches rather than scored rows.
_warming = False


def settle(status: Callable[[], Mapping[str, Any]]) -> Optional[dict[str, Any]]:
    """Wait out the file watcher's re-embedding burst before scoring (macOS).

    On macOS, FSEvents reports the files of the just-copied evidence tree to
    AFT's watcher as changed, about a second after the semantic index reports
    ready; the watcher then drops and re-embeds those entries while the index
    still says `ready`, so the first rows are ranked against a partly empty
    index (see "Which embeddings the gate uses" in this directory's README.md).
    With
    `AFT_SEARCH_BENCH_SETTLE_SECONDS=N` set, this waits at least N seconds
    after ready, and then until the semantic entry count has stopped changing
    and nothing is refreshing for five seconds. Unset, it returns at once.
    """
    raw = os.environ.get(SETTLE_ENV, "").strip()
    if not raw:
        return None
    minimum = float(raw)
    started = time.monotonic()
    stable_since: Optional[float] = None
    last: tuple[Any, Any, Any] = (None, None, None)
    peak = 0
    while True:
        semantic = status().get("semantic_index", {})
        entries = semantic.get("entries") if isinstance(semantic, Mapping) else None
        current = (
            semantic.get("status") if isinstance(semantic, Mapping) else None,
            entries,
            semantic.get("refreshing_count") if isinstance(semantic, Mapping) else None,
        )
        if isinstance(entries, int):
            peak = max(peak, entries)
        now = time.monotonic()
        quiet = current[0] == "ready" and current[2] in (0, None) and entries == peak
        if current != last or not quiet:
            stable_since = now if quiet else None
            last = current
        if stable_since is not None and now - stable_since >= 5.0 and now - started >= minimum:
            return {"settle_seconds": round(now - started, 3), "entries": entries}
        if now - started > max(minimum, 0.0) + 900.0:
            raise TimeoutError(f"semantic_index_never_settled:{current}")
        time.sleep(0.5)


def rerank_block() -> Optional[dict[str, Any]]:
    raw = os.environ.get(CONFIG_ENV, "").strip()
    if not raw:
        return None
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise ValueError(f"{CONFIG_ENV} must be a JSON object")
    return value


def apply_to_config(doc: MutableMapping[str, Any]) -> MutableMapping[str, Any]:
    """Add the configured `search.rerank` block to a user-tier config document."""
    block = rerank_block()
    if block is not None:
        doc["search"] = {"rerank": block}
    return doc


def apply_to_env(env: MutableMapping[str, str]) -> MutableMapping[str, str]:
    """Point the AFT process at the reranker model cache, when one is named."""
    cache = os.environ.get(CACHE_ENV, "").strip()
    if cache:
        env["FASTEMBED_CACHE_DIR"] = cache
    return env


def skip_note(response: Mapping[str, Any]) -> Optional[str]:
    match = _NOTE.search(str(response.get("text", "")))
    return match.group(1) if match else None


def observe(runner: str, arguments: Mapping[str, Any], response: Mapping[str, Any], elapsed_ms: float) -> None:
    path = os.environ.get(LOG_ENV, "").strip()
    if not path:
        return
    record = {
        "runner": runner,
        "query": arguments.get("query"),
        "topK": arguments.get("topK", arguments.get("top_k")),
        "offset": arguments.get("offset", 0),
        "elapsed_ms": round(elapsed_ms, 3),
        "note": skip_note(response),
        "success": response.get("success"),
        "warmup": _warming,
    }
    with Path(path).open("a", encoding="utf-8") as log:
        log.write(json.dumps(record, sort_keys=True) + "\n")


def burn_in_queries() -> list[str]:
    """Prose queries for warming the reranker that no scored suite uses.

    They are the rows of `split-tuning-manifest.json`, which exist for tuning
    and never enter a scored suite. Their query vectors are in the vector pack
    the concept and real-query replays serve from, so the fixture embedding
    server can embed them instead of refusing an unknown text.
    """
    manifest = json.loads((Path(__file__).resolve().parent / "split-tuning-manifest.json").read_text())
    return [str(row["query"]) for row in manifest["rows"]]


def warm_up(
    search: Callable[[str], Mapping[str, Any]],
    queries: Iterable[str],
    timeout: float = 600.0,
    drain_seconds: float = 20.0,
) -> Optional[dict[str, Any]]:
    """Wait for the reranker to be installed and to score once; report the time.

    AFT starts building the backend when the configuration is published, so it
    may be ready by the time the index is. Each burn-in query is searched until
    its note is no longer "backend not ready". The first inference of a fresh
    session can be much slower than later ones; when it ends in a "timeout"
    skip, or a "busy" skip (the ONNX worker still computing an earlier
    request), the worker may still be computing, so this waits `drain_seconds`
    and tries the next burn-in query, until one is answered without a note.
    AFT remembers the first outcome (an order or a skip) for each result list
    and replays it for that list afterwards; burn-in queries are never scored,
    so an outcome remembered for one of them affects no row. Returns None when
    no reranker is configured.

    A query that does not reach the reranker (a regex route, fewer than two
    candidates) also carries no note, so a scored row can still meet a backend
    that is not ready; the request log records every row's note, which is
    where that is checked.
    """
    if rerank_block() is None:
        return None
    global _warming
    _warming = True
    try:
        return _warm_up(search, queries, timeout, drain_seconds)
    finally:
        _warming = False


def _warm_up(
    search: Callable[[str], Mapping[str, Any]],
    queries: Iterable[str],
    timeout: float,
    drain_seconds: float,
) -> dict[str, Any]:
    started = time.monotonic()
    deadline = started + timeout
    steps: list[dict[str, Any]] = []
    for query in queries:
        while True:
            began = time.monotonic()
            note = skip_note(search(query))
            elapsed_ms = round((time.monotonic() - began) * 1000.0, 3)
            if note != _NOT_READY:
                break
            if time.monotonic() > deadline:
                raise TimeoutError(f"rerank_backend_not_ready_after:{timeout}s")
            time.sleep(0.2)
        steps.append({"query": query, "note": note, "elapsed_ms": elapsed_ms})
        if note is None:
            break
        time.sleep(drain_seconds)
    return {"warmup_seconds": round(time.monotonic() - started, 3), "steps": steps}


def report_warm_up(runner: str, outcome: Optional[Mapping[str, Any]]) -> None:
    if outcome is None:
        return
    print(f"rerank_warmup:{runner}:{json.dumps(outcome, sort_keys=True)}", flush=True)
    path = os.environ.get(LOG_ENV, "").strip()
    if path:
        with Path(path).open("a", encoding="utf-8") as log:
            log.write(json.dumps({"runner": runner, "warmup": dict(outcome)}, sort_keys=True) + "\n")
