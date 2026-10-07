# Search capability digest and reference re-recording

The real-query harness used to hash all of
`packages/pi-plugin/src/tools/semantic.ts` while reading only its `SearchParams`
declaration. An unrelated Pi renderer edit therefore changed the capability
digest and failed an `engine_unwired` comparison even when every search result
was identical.

The harness now hashes the extracted TypeScript schema block, normalizing only
line endings. JSON schemas use the harness's canonical JSON encoding. The
`schema_sha256` field and the remaining capability fields keep their existing
names and meaning. Goldens cover edits outside the declaration, new parameters,
bounds and descriptions, CRLF/LF parity, significant schema whitespace, and JSON
formatting/key order. Restoring whole-file hashing makes the outside-edit golden
fail while the schema-edit control passes; the equivalent JSON mutation makes
the canonical-schema golden fail while its schema-edit control passes.

## Recording

The release binary was built from unchanged `origin/main` at
`2772a315f207a1787be34fba3557ad2013d0f565`, on macOS, with the Rust compiler
wrappers disabled. No engine, ranking-fence file, parameter declaration,
manifest, or vector pack changed.

```bash
CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo build --release -p agent-file-tools
python3 benchmarks/aft-search/provision_corpus.py
python3 benchmarks/aft-search/provision_evidence.py
AFT_BINARY_PATH=target/release/aft scripts/telemetry/cost-gate.sh --search-quality --mode record-reference
```

The recording ran 16 exact-recall fixtures, 26 concept-recall fixtures, and all
93 real-query rows. Comparing the old and new JSON documents gives exactly
three changed fields:

| Field | Classification | Reason |
|---|---|---|
| `capability.schema_sha256` | Capability digest | Now hashes the parameter declaration, not the plugin file |
| `baseline_sha256` | Recording provenance | Hash of the previous reference being replaced |
| `binary_sha256` | Recording provenance | Hash of this freshly built release binary |

The provenance fields were retained as generated, not hand-edited to conceal
the new recording. The sidecar's `reference_sha256` changed to bind the newly
recorded reference; its `manifest_sha256` stayed unchanged.

All 93 rows are byte-identical under canonical JSON; all family metrics, fixture
groups, shapes, mechanisms, paired deltas, and other fields are exactly equal.
The harness's `real_query_behavior_diff` excludes recording provenance, and its
entire behavioral change is:

```diff
-    "schema_sha256": "a04b75ef79d47cff40bd5b4cd4d23b401ccef006addd4de62d5f2017e6848daf"
+    "schema_sha256": "6dfa7a90514869071a984f74bb504cf6a055ad5da5a38de5b3d4924d874dd17f"
```

Real-query hit@1 / hit@5 / MRR@10 remain
`0.3010752688172043 / 0.5268817204301075 / 0.3948924731182796`.
