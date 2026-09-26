#!/usr/bin/env python3
"""Concept-recall runner: rank scoring, vector refusal and determinism.

These cases need no aft binary. The replay itself is exercised end to end by
`run_concept_recall.py` inside the search-quality gate.
"""
from __future__ import annotations

import json
import tempfile
import threading
import unittest
from pathlib import Path
from typing import Any, Mapping

from embedding_fixture_server import Server, query_key
from run_concept_recall import (
    DEFAULT_QUERY_PACK,
    INCLUDE_TESTS,
    assemble,
    check_query_pack,
    load_fixtures,
    require_query_vectors,
    score_cases,
)
from search_quality_lib import PAGE_SIZE, InputFault, canonical_json, sha256_file
from test_run_real_query import _post_embeddings
from vector_pack import read_pack

HERE = Path(__file__).resolve().parent
TEMPLATE = "aft-search-template-v1"


def fixtures() -> list[dict[str, Any]]:
    return [
        {"query": "answer third", "shape": "natural-language", "expected_top_files": ["src/answer.rs", "src/alt.rs"]},
        {"query": "answer first", "shape": "identifier", "expected_top_files": ["src/answer.rs"]},
        {"query": "answer absent", "shape": "identifier", "expected_top_files": ["src/nowhere.rs"]},
    ]


class RankedClient:
    """Returns a fixed ranking per query, with repeated files like real replies."""

    RANKINGS = {
        "answer third": ["src/a.rs", "src/a.rs", "src/b.rs", "src/alt.rs", "src/answer.rs"],
        "answer first": ["src/answer.rs", "src/b.rs"],
        "answer absent": [f"src/f{index}.rs" for index in range(12)] + ["src/nowhere.rs"],
    }

    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    def search(self, arguments: Mapping[str, Any]) -> dict[str, Any]:
        self.calls.append(dict(arguments))
        paths = self.RANKINGS[str(arguments["query"])]
        return {
            "success": True,
            "status": "ready",
            "results": [{"file": path} for path in paths],
            "structuredContent": {"plan": {"shape": "fixture", "lanes_run": ["lexical", "semantic"]}},
        }


class ConceptRecallTests(unittest.TestCase):
    def test_case_scores_the_rank_of_its_first_answer_file(self) -> None:
        client = RankedClient()
        rows = score_cases(fixtures(), client, Path("."), lambda: [])
        by_query = {row["query"]: row for row in rows}
        # Repeated files collapse, so src/alt.rs is the third distinct file.
        self.assertEqual(by_query["answer third"]["answer_rank"], 3)
        self.assertEqual(by_query["answer third"]["answer_file"], "src/alt.rs")
        self.assertEqual(by_query["answer third"]["mrr_at_10"], 1.0 / 3)
        self.assertEqual(by_query["answer third"]["hit_at_5"], 1.0)
        self.assertEqual(by_query["answer third"]["hit_at_1"], 0.0)
        self.assertEqual(by_query["answer first"]["mrr_at_10"], 1.0)
        # The answer is the thirteenth distinct file: outside the scored ten.
        self.assertEqual(by_query["answer absent"]["answer_rank"], 0)
        self.assertEqual(by_query["answer absent"]["mrr_at_10"], 0.0)
        self.assertEqual(len(by_query["answer absent"]["ranked_paths"]), 10)
        self.assertEqual(by_query["answer third"]["lanes_run"], ["lexical", "semantic"])
        self.assertEqual(
            client.calls[0], {"query": "answer third", "topK": PAGE_SIZE, "includeTests": INCLUDE_TESTS}
        )
        score = assemble(rows, "fixture-model", {})
        self.assertAlmostEqual(score["metrics"]["mrr_at_10"], (1.0 / 3 + 1.0) / 3)
        self.assertAlmostEqual(score["metrics"]["hit_at_1"], 1.0 / 3)

    def test_score_is_byte_deterministic_for_the_same_replies(self) -> None:
        outputs = [
            canonical_json(assemble(score_cases(fixtures(), RankedClient(), Path("."), lambda: []), "m", {}))
            for _ in range(2)
        ]
        self.assertEqual(outputs[0], outputs[1])

    def test_missing_query_vector_is_refused_before_aft_starts(self) -> None:
        vectors = {query_key("answer third", TEMPLATE): [0.1], query_key("answer first", TEMPLATE): [0.2]}
        with self.assertRaisesRegex(InputFault, "vector_missing:" + query_key("answer absent", TEMPLATE)):
            require_query_vectors(fixtures(), vectors, TEMPLATE)
        vectors[query_key("answer absent", TEMPLATE)] = [0.3]
        require_query_vectors(fixtures(), vectors, TEMPLATE)

    def test_vector_refused_during_the_replay_fails_the_run(self) -> None:
        refused: list[str] = []
        client = RankedClient()
        original = client.search

        def search(arguments: Mapping[str, Any]) -> dict[str, Any]:
            if arguments["query"] == "answer first":
                refused.append("query:missing|corpus:missing")
            return original(arguments)

        client.search = search  # type: ignore[method-assign]
        with self.assertRaisesRegex(InputFault, "vector_missing:answer first:query:missing"):
            score_cases(fixtures(), client, Path("."), lambda: refused)

    def test_fixture_server_records_the_keys_it_refuses(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            vectors = {query_key("known", TEMPLATE): [0.5, 0.5]}
            server = Server(("127.0.0.1", 0), vectors, TEMPLATE, Path(directory) / "requests.log")
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                self.assertEqual(_post_embeddings(server.server_port, ["known"])[0], 200)
                self.assertEqual(server.refused, [])
                self.assertEqual(_post_embeddings(server.server_port, ["unknown"])[0], 422)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)
            self.assertEqual(len(server.refused), 1)
            self.assertTrue(server.refused[0].startswith(query_key("unknown", TEMPLATE) + "|corpus:"))

    def test_query_pack_from_another_model_or_corpus_pack_is_refused(self) -> None:
        corpus = {"pinned_sha": "p", "embed_template_version": TEMPLATE, "model_id": "m", "dimension": 384}
        matching = {**corpus, "corpus_pack_sha256": "d"}
        check_query_pack(matching, corpus, "d")
        with self.assertRaisesRegex(InputFault, "concept_query_pack:model_id"):
            check_query_pack({**matching, "model_id": "hashed-stand-in"}, corpus, "d")
        with self.assertRaisesRegex(InputFault, "concept_query_pack:corpus_pack_sha256"):
            check_query_pack(matching, corpus, "other")

    def test_committed_query_pack_covers_every_fixture_and_binds_the_real_query_pack(self) -> None:
        manifest_path = HERE / "real-query-manifest.json"
        rows = json.loads(manifest_path.read_text())["rows"]
        included = [row for row in rows if "excluded_reason" not in row]
        corpus_pack_path = HERE.parents[1] / included[0]["embedding_pack"]
        corpus_pack = read_pack(corpus_pack_path)
        query_pack = read_pack(DEFAULT_QUERY_PACK)
        check_query_pack(query_pack, corpus_pack, sha256_file(corpus_pack_path))
        cases = load_fixtures(HERE / "fixtures.json")
        require_query_vectors(cases, query_pack["vectors"], TEMPLATE)
        self.assertEqual(len(query_pack["vectors"]), len(cases))


if __name__ == "__main__":
    unittest.main()
