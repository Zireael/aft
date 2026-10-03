#!/usr/bin/env python3
"""Embed query/pattern fixture texts while preserving real-query-vectors.bin."""
from __future__ import annotations

import argparse
import json
from pathlib import Path

from embedding_fixture_server import query_key
from minilm_embedder import MiniLmEmbedder
from run_real_query import ROOT, HERE
from search_quality_lib import canonical_json, sha256_file
from vector_pack import read_pack, write_pack


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-vector-authoring", action="store_true", required=True)
    parser.add_argument(
        "--tuning-only",
        action="store_true",
        help="write split-tuning-vectors.bin for the tuning manifest alone, leaving the gate's pack and manifest untouched",
    )
    args = parser.parse_args()
    if not args.allow_vector_authoring:
        parser.error("authoring must be explicit")
    paths = [HERE / "real-query-manifest.json", HERE / "split-tuning-manifest.json"]
    output_name = "split-query-vectors.bin"
    if args.tuning_only:
        paths = [HERE / "split-tuning-manifest.json"]
        output_name = "split-tuning-vectors.bin"
    manifests = [json.loads(path.read_text()) for path in paths]
    pack = read_pack(HERE / "real-query-vectors.bin")
    texts = {row["query"] for manifest in manifests for row in manifest["rows"] if "pattern" in row}
    texts.update(row["query"] + " " + row["pattern"] for manifest in manifests for row in manifest["rows"] if "pattern" in row and row["pattern"].strip())
    embedder = MiniLmEmbedder()
    vectors = {query_key(text, pack["embed_template_version"]): embedder.embed([text])[0] for text in sorted(texts)}
    output = HERE / output_name
    metadata = {key: value for key, value in pack.items() if key not in {"vectors", "count", "schema", "dtype"}}
    metadata["source"] = embedder.source
    write_pack(output, metadata, vectors)
    binding = {"path": output.relative_to(ROOT).as_posix(), "sha256": sha256_file(output)}
    for path, manifest in zip(paths, manifests):
        manifest["split_query_pack"] = binding
        path.write_bytes(canonical_json(manifest))
    print(f"split_query_vectors:{len(vectors)} sha256:{binding['sha256']}")


if __name__ == "__main__":
    main()
