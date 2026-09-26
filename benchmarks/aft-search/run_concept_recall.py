#!/usr/bin/env python3
"""Replay the concept-recall fixtures through standalone AFT and score answer ranks.

Each case in `fixtures.json` is sent to AFT's public `search` tool, with
semantic search on, over the same pinned evidence tree and with the same
embedding service the real-query gate uses: `embedding_fixture_server.py`
serving real all-MiniLM-L6-v2 vectors. Chunk vectors come from the real-query
pack (`real-query-vectors.bin`); the vectors for the concept queries
themselves come from `concept-query-vectors.bin`, a pack in the same format
captured the same way (`--capture`). Keeping the concept queries in their own
pack leaves the real-query pack, and the manifest rows bound to its digest,
untouched.

A case scores the rank of the first of its `expected_top_files` among the
first ten distinct files AFT returns. The router decides which lanes a query
runs, as it does for users; each row records `lanes_run`, so a reader can see
which cases the semantic lane actually served.

The run refuses instead of guessing. Every query must have a stored vector
before AFT starts, and a text the embedding service had to refuse during the
run (`vector_missing`) fails the run, because AFT would otherwise answer that
query without its semantic lane and the score would look ordinary.

On macOS the replay is not repeatable yet, for the watcher reason the README
gives for the real-query replay; record on Linux.
"""
from __future__ import annotations

import argparse
import json
import sys
import tempfile
import threading
from collections import ChainMap
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Callable, Iterator, Mapping, MutableMapping, Sequence

from embedding_fixture_server import Server, query_key
from run_real_query import (
    DEFAULT_BINARY,
    FIXTURE_PROVIDER_MODEL,
    AftProtocolError,
    NdjsonClient,
    UnsupportedPlatform,
    PLATFORM_UNSUPPORTED_EXIT,
    _normalize_results,
    assert_reference_platform,
    ensure_binary,
    load_inputs,
    runtime_evidence_tree,
)
from search_quality_lib import (
    EVIDENCE_SHA,
    METRICS,
    PAGE_SIZE,
    InputFault,
    canonical_json,
    collapse_paths,
    mean_metrics,
    sha256_file,
)
from vector_pack import read_pack, write_pack

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
SCHEMA = "aft-search-concept-score-v2"
DEFAULT_QUERY_PACK = HERE / "concept-query-vectors.bin"
# The product default. Several concept queries (LSPManager, subagent_type,
# aft_safety_history) appear verbatim in the pinned tree's query-shape test
# files, which would answer them by string match. Three cases also list a test
# file among their answers (the useState pair and SemanticIndexFingerprint);
# each of them lists a non-test file too, so every case stays answerable.
INCLUDE_TESTS = False
JsonObject = dict[str, Any]


def load_fixtures(path: Path) -> list[JsonObject]:
    fixtures = json.loads(path.read_text())
    if not isinstance(fixtures, list) or not fixtures:
        raise InputFault(f"malformed_schema:{path}")
    queries = [str(fixture.get("query", "")) for fixture in fixtures]
    if any(not query for query in queries) or len(set(queries)) != len(queries):
        raise InputFault(f"malformed_schema:{path}:queries")
    for fixture in fixtures:
        expected = fixture.get("expected_top_files")
        if not isinstance(expected, list) or not expected or not all(isinstance(item, str) and item for item in expected):
            raise InputFault(f"malformed_schema:{path}:expected_top_files:{fixture['query']}")
    return fixtures


def check_query_pack(query_pack: Mapping[str, Any], corpus_pack: Mapping[str, Any], corpus_pack_sha256: str) -> None:
    """The concept pack must come from the same model, pin and corpus pack."""
    for field in ("pinned_sha", "embed_template_version", "model_id", "dimension"):
        if query_pack.get(field) != corpus_pack.get(field):
            raise InputFault(f"corpus_vector_model_mismatch:concept_query_pack:{field}")
    if query_pack.get("corpus_pack_sha256") != corpus_pack_sha256:
        raise InputFault("corpus_vector_model_mismatch:concept_query_pack:corpus_pack_sha256")


def require_query_vectors(fixtures: Sequence[Mapping[str, Any]], vectors: Mapping[str, Any], template: str) -> None:
    """Refuse before AFT starts when any fixture query has no stored vector."""
    missing = [query_key(str(fixture["query"]), template) for fixture in fixtures]
    missing = [key for key in missing if key not in vectors]
    if missing:
        raise InputFault("vector_missing:" + ",".join(missing))


def require_answer_files(fixtures: Sequence[Mapping[str, Any]], project_root: Path) -> None:
    """An expected file absent from the tree could never rank; that is a fixture fault."""
    for fixture in fixtures:
        for path in fixture["expected_top_files"]:
            if not (project_root / path).is_file():
                raise InputFault(f"concept_answer_missing:{fixture['query']}:{path}")


def _plan(response: Mapping[str, Any]) -> Mapping[str, Any]:
    structured = response.get("structuredContent")
    plan = structured.get("plan") if isinstance(structured, Mapping) else None
    return plan if isinstance(plan, Mapping) else {}


def score_case(fixture: Mapping[str, Any], response: Mapping[str, Any], project_root: Path) -> JsonObject:
    ranked_paths = collapse_paths(_normalize_results(response, project_root))
    expected = [str(path) for path in fixture["expected_top_files"]]
    rank = next((index for index, path in enumerate(ranked_paths, 1) if path in expected), 0)
    plan = _plan(response)
    return {
        "query": fixture["query"],
        "fixture_group": fixture.get("shape", "unknown"),
        "expected_top_files": expected,
        "ranked_paths": ranked_paths,
        "answer_rank": rank,
        "answer_file": ranked_paths[rank - 1] if rank else None,
        "router_shape": plan.get("shape"),
        "lanes_run": plan.get("lanes_run"),
        "mrr_at_10": 1.0 / rank if rank else 0.0,
        "hit_at_1": float(rank == 1),
        "hit_at_5": float(0 < rank <= 5),
    }


def score_cases(
    fixtures: Sequence[Mapping[str, Any]],
    client: Any,
    project_root: Path,
    refused: Callable[[], Sequence[str]],
) -> list[JsonObject]:
    rows: list[JsonObject] = []
    for fixture in fixtures:
        request = {"query": fixture["query"], "topK": PAGE_SIZE, "includeTests": INCLUDE_TESTS}
        response = client.search(request)
        missing = list(refused())
        if missing:
            raise InputFault(f"vector_missing:{fixture['query']}:" + ",".join(missing))
        rows.append(score_case(fixture, response, project_root))
    return rows


def assemble(rows: Sequence[Mapping[str, Any]], model_id: str, provenance: Mapping[str, Any]) -> JsonObject:
    return {
        "schema": SCHEMA,
        "evidence_sha": EVIDENCE_SHA,
        "model_id": model_id,
        **provenance,
        "request": {"topK": PAGE_SIZE, "includeTests": INCLUDE_TESTS},
        "rows": [dict(row) for row in rows],
        "metrics": mean_metrics([{metric: row[metric] for metric in METRICS} for row in rows]),
    }


@contextmanager
def vector_server(vectors: Mapping[str, Any], template: str, log: Path, **options: Any) -> Iterator[Server]:
    server = Server(("127.0.0.1", 0), vectors, template, log, **options)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def _display(path: Path) -> str:
    try:
        return path.resolve().relative_to(ROOT).as_posix()
    except ValueError:
        return path.resolve().as_posix()


def run(args: argparse.Namespace) -> int:
    assert_reference_platform()
    fixtures_path = Path(args.fixtures).resolve()
    manifest_path = Path(args.manifest).resolve()
    query_pack_path = Path(args.query_pack).resolve()
    binary = Path(args.binary).resolve()
    fixtures = load_fixtures(fixtures_path)
    _manifest, tree, corpus_pack_path, corpus_pack = load_inputs(manifest_path)
    corpus_pack_sha256 = sha256_file(corpus_pack_path)
    if not query_pack_path.is_file():
        raise InputFault(f"vector_missing:concept_query_pack:{_display(query_pack_path)}")
    query_pack = read_pack(query_pack_path)
    check_query_pack(query_pack, corpus_pack, corpus_pack_sha256)
    template = str(corpus_pack["embed_template_version"])
    vectors = ChainMap(query_pack["vectors"], corpus_pack["vectors"])
    require_query_vectors(fixtures, vectors, template)
    ensure_binary(binary)
    with tempfile.TemporaryDirectory(prefix="aft-concept-recall-run-") as run_dir, runtime_evidence_tree(tree) as project_root:
        require_answer_files(fixtures, project_root)
        runtime = Path(run_dir)
        with vector_server(vectors, template, runtime / "embedding-requests.log") as server:
            client = NdjsonClient(binary, project_root, runtime / "storage", runtime / "aft.stderr")
            try:
                client.configure(f"http://127.0.0.1:{server.server_port}", FIXTURE_PROVIDER_MODEL, args.ready_timeout)
                client.wait_ready(args.ready_timeout)
                if server.refused:
                    raise InputFault("vector_missing:index:" + ",".join(server.refused))
                rows = score_cases(fixtures, client, project_root, lambda: server.refused)
            finally:
                client.close()
    provenance = {
        "fixtures_path": _display(fixtures_path),
        "fixtures_sha256": sha256_file(fixtures_path),
        "corpus_pack_path": _display(corpus_pack_path),
        "corpus_pack_sha256": corpus_pack_sha256,
        "query_pack_path": _display(query_pack_path),
        "query_pack_sha256": sha256_file(query_pack_path),
        "binary_sha256": sha256_file(binary),
    }
    data = canonical_json(assemble(rows, str(corpus_pack["model_id"]), provenance))
    if args.output:
        output = Path(args.output)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(data)
        print(f"concept_recall_rows:{len(rows)}")
        print(f"concept_recall_score:{output}")
        print(f"concept_recall_score_sha256:{sha256_file(output)}")
    else:
        sys.stdout.buffer.write(data)
    return 0


def capture(args: argparse.Namespace) -> int:
    """Author `concept-query-vectors.bin` from the texts AFT embeds for the cases.

    AFT indexes the pinned tree against the real-query pack and then runs each
    case, so the pack receives exactly the query texts AFT sends, embedded by
    `minilm_embedder` as the real-query capture does. Only a text that is one of
    the fixture queries may be recorded: a chunk the real-query pack lacks means
    that pack is stale, and recording it here would hide that.
    """
    from minilm_embedder import MODEL_ID, MiniLmEmbedder

    fixtures_path = Path(args.fixtures).resolve()
    fixtures = load_fixtures(fixtures_path)
    manifest_path = Path(args.manifest).resolve()
    _manifest, tree, corpus_pack_path, corpus_pack = load_inputs(manifest_path)
    if corpus_pack.get("model_id") != MODEL_ID:
        raise InputFault("corpus_vector_model_mismatch:embedding_pack:model_id")
    binary = Path(args.binary).resolve()
    if not binary.is_file():
        raise InputFault(f"aft_binary_missing:{binary}")
    template = str(corpus_pack["embed_template_version"])
    queries = {str(fixture["query"]) for fixture in fixtures}
    embedder = MiniLmEmbedder()
    recorded: dict[str, list[float]] = {}
    unexpected: list[str] = []

    def record(text: str) -> list[float]:
        if text not in queries:
            unexpected.append(text[:120])
            raise ValueError("vector_missing:not_a_concept_query")
        return embedder.embed([text])[0]

    store: MutableMapping[str, Any] = ChainMap(recorded, corpus_pack["vectors"])
    with tempfile.TemporaryDirectory(prefix="aft-concept-vector-authoring-") as run_dir, runtime_evidence_tree(tree) as project_root:
        runtime = Path(run_dir)
        with vector_server(store, template, runtime / "requests.log", recorder=record, query_texts=queries) as server:
            client = NdjsonClient(binary, project_root, runtime / "storage", runtime / "aft.stderr")
            try:
                client.configure(f"http://127.0.0.1:{server.server_port}", FIXTURE_PROVIDER_MODEL, args.ready_timeout)
                client.wait_ready(args.ready_timeout)
                for fixture in fixtures:
                    client.search({"query": fixture["query"], "topK": PAGE_SIZE, "includeTests": INCLUDE_TESTS})
            finally:
                client.close()
    if unexpected:
        raise InputFault("concept_capture_unexpected_text:" + " | ".join(unexpected))
    requested_by_aft = len(recorded)
    # The router skips the semantic lane for some shapes, so AFT never asks for
    # those queries. Embed them directly: a later router change that sends them
    # to the semantic lane must find a vector rather than a refusal.
    for query in sorted(queries):
        key = query_key(query, template)
        if key not in recorded:
            recorded[key] = embedder.embed([query])[0]
    header = {
        "pinned_sha": EVIDENCE_SHA,
        "embed_template_version": template,
        "model_id": MODEL_ID,
        "source": embedder.source,
        "corpus_pack_sha256": sha256_file(corpus_pack_path),
    }
    output = Path(args.query_pack).resolve()
    write_pack(output, header, recorded)
    print(f"captured_concept_vectors:{len(recorded)}")
    print(f"captured_concept_vectors_requested_by_aft:{requested_by_aft}")
    print(f"captured_concept_vector_pack:{output}")
    print(f"captured_concept_vector_pack_sha256:{sha256_file(output)}")
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    result.add_argument("--fixtures", default=str(HERE / "fixtures.json"))
    result.add_argument("--manifest", default=str(HERE / "real-query-manifest.json"))
    result.add_argument("--query-pack", default=str(DEFAULT_QUERY_PACK))
    result.add_argument("--binary", default=DEFAULT_BINARY)
    result.add_argument("--ready-timeout", type=float, default=600.0)
    result.add_argument("--output")
    result.add_argument(
        "--capture",
        action="store_true",
        help=(
            "Authoring only: write --query-pack from the real model instead of scoring. Needs the "
            "model cache and onnxruntime/tokenizers/numpy (see the README)."
        ),
    )
    return result


def main() -> int:
    try:
        args = parser().parse_args()
        return capture(args) if args.capture else run(args)
    except UnsupportedPlatform as error:
        print(str(error), file=sys.stderr)
        return PLATFORM_UNSUPPORTED_EXIT
    except (AftProtocolError, InputFault, OSError, KeyError, ValueError, json.JSONDecodeError) as error:
        print(str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
