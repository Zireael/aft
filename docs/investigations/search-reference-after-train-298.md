# Search-quality reference re-recorded after the prose-preview change

The prose-preview fix restored symbol locations and code snippets in prose
search results. It changed only the rendered summary text of ten split-query
rows; their ranked paths and every score are unchanged, which the descriptor's
`presentation_rows` allowance already pinned when that change landed.

The reference was recorded from a release build of the landed main commit
`2bb6bef81abf8eaf2688466ea264facf28f82ee0` with
`AFT_BINARY_PATH=target/release/aft scripts/telemetry/cost-gate.sh --search-quality --mode record-reference`
(corpus provisioned with `benchmarks/aft-search/provision_corpus.py`).

Compared with the previous reference, the only changed fields are `summary_text`
on rows 81, 83, 84, 85, 86, 87, 88, 89, 91 and 92, where a bare
`path [lexical match]` line became the symbol header and its snippet, plus the
binary and baseline digests. No ranked path, score or summary metric moved.

Without this re-record, every later train that touches a ranking-fenced file and
declares `engine_unwired` would be byte-compared against the old summaries and
fail with `engine_unwired_mismatch`.
