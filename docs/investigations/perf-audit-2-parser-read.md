# Parser, tokenizer, read and navigation performance audit

Reviewed against `52f09206136a11e1cd18ebf8348d58200bae6e55`. Locations in the
finding column are the audit/base locations; implementation locations name the
new helpers where appropriate. Unmeasured findings below are **not** claimed as
fixed. `commands/outline.rs` and the public-only `symbol_diff.rs` finding were
outside this slice.

## Findings and deterministic work measurements

| Finding / location | Verdict and implementation evidence | Before → after | Test / evidence |
| --- | --- | --- | --- |
| #11: `parser.rs:2735,2888,4498` | Confirmed and fixed. `VariableNameIndex::should_capture` rejects ineligible declarations before membership work and incrementally indexes the append-only symbol suffix. TS/JS retain module-scope rules. Zig had no module-scope predicate; its existing const/container predicates are preserved rather than changing which symbols are extracted. | TS: **24,006,001 → 7,999**; JS: **240,601 → 799**; Zig: **79,800 → 799** name operations. Baseline counts string comparisons; optimized counts insertions plus membership probes. | `parser::tests::variable_name_membership_is_linear_and_byte_identical`: serialized symbol vectors match the original scan. TS fixture has 4,000 top-level and 4,000 local const declarations. |
| #10: `aft-tokenizer/src/claude.rs:88-111` | Confirmed and fixed. Stable byte-offset linked boundaries plus a lazy-invalidated minimum heap replace minimum scans and vector shifting. Original leftmost tie-breaking is preserved. The old implementation remains a test-only oracle. | 8 KiB `=`: **78,286,784 → 331,111**; 8 KiB newlines: **78,229,376 → 293,011**; mixed whitespace: **37,754,880 → 104,969**; normal text: **1,680 → 100** comparisons/shifts. Optimized counts actual heap comparisons; there are no vector shifts. | `claude::tests::bpe_heap_work_is_bounded_and_tokens_are_identical`: exact token IDs and counts agree on 132 corpus entries, including punctuation, blank lines, Unicode and normal code/text. Also exercises 128 KiB punctuation and newline inputs. |
| Cached tables: `parser.rs:1811,1344,1842` | Confirmed; fixed for read-only resolver/zoom/container consumers through `extract_symbols_shared` and `list_symbols_shared`. Cold shared extraction also inserts the same Arc rather than copying the table. Owned compatibility APIs intentionally still return owned vectors; outline/prewarm callers outside ownership were not changed. | **1 full-table copy per fresh owned hit → 0** for shared hits/resolution. Ten hits plus ten resolutions of a 1,000-symbol file retain the same table allocation. | `parser::tests::shared_symbol_hits_and_resolution_do_not_copy_the_table`; the existing owned-cache test remains unchanged and green. |
| #18: `calls.rs:557-657` | Confirmed and fixed. `RustValueWalk` carries cursor ancestry instead of asking for each identifier's parent. `rust_block_bindings` indexes each block's earliest let binding once, replacing reverse sibling walks. Outside ancestry is captured once for subtree callers. Parameter, pattern, closure and conditional scope rules remain unchanged. | **3,000,033 → 7,063** block binding operations on a 1,000-declaration callback-heavy Rust function plus adversarial scope cases. Baseline counts preceding sibling visits; optimized counts block children/pattern nodes and binding probes. | `calls::tests::rust_value_binding_work_is_linear_and_results_are_identical`: complete reference tuples, including byte ranges, match the old extractor. |
| Zoom enrichment: `commands/zoom.rs:1166-1175,1483,1490` | Confirmed; redundant disk reads fixed by `zoom_parse_source`, which parses the source already displayed. Re-export enrichment also reuses the already-read resolved source. JSON path and call enrichment still need syntax trees; this does not claim to eliminate all JSON parses. | Warm callgraph enrichment: **1 extra file read → 0**, identical response. | `commands::zoom::tests::zoom_enrichment_reuses_source_bytes_and_matches_legacy_output`, plus existing JSON and batched zoom suites. |
| Hashline read: `commands/read.rs:1064,1139` | Confirmed and fixed for whole-file text reads. `handle_read_local` passes a coherent `ReadSource` to the snapshot adapter and computes legacy response metadata without constructing a discarded body. It also reuses the validated path. Explicit streaming ranges, media/artifacts and declined bash rewrites retain their existing paths. | Whole text/CRLF/read-only/empty files: **2 body reads → 1**. Large 20,000-line log: **51,171 discarded rendered bytes → 0**. Binary/invalid UTF-8: **1 → 1**. | `commands::read::hashline_wiring_tests::hashline_whole_read_uses_one_source_and_matches_legacy_responses_and_snapshots`: byte-identical response serialization and exact stored snapshots for normal, CRLF, empty, read-only, binary, invalid UTF-8, large and wide-line fixtures; missing paths remain refused and store-neutral. |
| Hashline source identity: `hashline/snapshot/mod.rs:read_source` | Added with explicit parent ownership approval. Disk capture is a thin wrapper around the shared-byte adapter: there is one refusal/render/publication rule set. Metadata comes from the same open file, before and after its read; changed size, stamp or permissions reject reuse. No later path stat is paired with earlier bytes. The ordinary read falls back to its existing disk path if coherent capture is unavailable. | **1 coherent body read**, with an identity fence; no timing assertion. | `hashline::snapshot::tests::read_source_rejects_identity_changes_during_the_read`: injected size-changing save after bytes are read is refused. |
| Symbol miss: `parser.rs:8285` | Confirmed and fixed. A missing local name in non-JS-family files returns before default/re-export source reading. | Warm Rust miss: **1 source read and 1 parse → 0 and 0**. | `parser::tests::non_js_symbol_miss_does_not_read_or_parse_again`; JS default/re-export tests remain green. |
| Indent leak: `indent.rs:27` | Confirmed and fixed. All u8 widths slice one static ASCII space buffer. | Repeated requests for exotic widths: **1 leaked allocation/call → 0**; all 256 widths have stable storage across 32 repetitions. | `indent::tests::indent_strings_reuse_storage_for_every_width`: exact strings match `repeat`, and the address census remains 256 instead of growing per call. |
| Disk persistence: `symbol_cache_disk.rs:153,378`; caller `commands/configure.rs:2377-2394` | Partly confirmed, unchanged. Changed-cache persistence still serializes under the caller's read lock. The stronger claim of a rewrite on every persist is inaccurate: `needs_persistence()` already skips unchanged caches. Correct lock-release work must also update that out-of-slice caller's revision/generation protocol. | Not measured; unchanged. | Inspected caller and writer; existing parser disk-cache tests passed. |
| Disk freshness: `parser.rs:1203` | Confirmed, deferred. `DecodedSymbolCache::read` verifies entries serially. | Not measured; unchanged. | Source inspection; parser disk-cache suite passed. |
| Repeated query passes: `parser.rs:2939,4071-4095` | Partly confirmed, deferred. Python decorated-definition collection and C++ type-name collection still precede their main query pass. The C extractor itself has one pass. | Not measured; unchanged. | Current Python/C/C++ extractor source; parser suite passed. |
| AST traversal/parsing: `commands/ast_scope.rs:227`, `ast_search.rs:133` | Partly incorrect. Scope walking is serial, but per-file AST search already uses Rayon `par_iter`. Lack of a literal prefilter and result cap is confirmed. A sound AST prefilter needs grammar-aware necessary literals; a new result cap would change the response contract. Neither was added. | Not measured; unchanged. | `ast_scope.rs:278-284`, `ast_search.rs:130-151,174-180`; AST unit suite passed. |
| AST replace: `commands/ast_replace.rs:340,358,365,427` | Partly confirmed, deferred. Replacement computation is already parallel; dry-run diff/syntax checking and backup/apply remain serial. Backup source reuse requires its shared safety protocol, not just removing a read. | Not measured; unchanged. | Existing AST replace suite passed; apply phase source inspected. |
| Signature copies: `parser.rs:2361,6561` | Confirmed, deferred. First-line strings are copied before the final signature cap. Early truncation must not alter consumers that inspect the unbounded signature, including Zig container classification. | Not measured; unchanged. | `extract_signature`, `extract_signature_between`, Zig classification; parser suite passed. |
| Rust child vectors: `parser.rs:3690` | Incorrect/already fixed at the base. `extract_rs_symbols_from_root` uses cursor traversal; it does not allocate a child vector per syntax node. | No such allocation at the claimed site. | Cursor-based Rust extractor; Rust parser tests passed. |
| Cache freshness: `parser.rs:1753,1083,1715` | Confirmed in part, deferred. Metadata may be checked twice. Rehashing is not unconditional: unchanged metadata and changed size already skip hashing. Shared-table changes preserve freshness and concurrent-refresh behavior. | Not measured; unchanged freshness work. | Existing `cached_freshness_*`, touched-cache and concurrent-replacement tests passed. |
| Kind serialization: `commands/symbol_render.rs:30` | Confirmed, deferred. `symbol_kind_string` still serializes the enum through a JSON value. Outline's duplicate site is excluded. | Not measured; unchanged. | Source inspection; zoom suite passed. |
| Whole-read line vector: `commands/read.rs:1435` | Confirmed, deferred. Whole-file metadata rendering still collects line references. Explicit ranged reads already use the streaming path. | Not measured; unchanged line-reference allocation. | Read suite passed; hashline optimization eliminates the discarded body, not this vector. |
| AST source lines: `commands/ast_search.rs:211` | Confirmed, deferred. Line references are collected even when the AST pattern finds no matches. | Not measured; unchanged. | AST search suite passed. |
| Import statement parsing: `imports/quotes.rs:41` | Confirmed, deferred. Individual statements are reparsed for module-string quote style; lexical quote scanning alone is not equivalent for comments and attributes. | Not measured; unchanged. | Import unit suite passed. |
| Inline extraction: `extract.rs:1415,1395,1258,1366` | Confirmed, deferred. Shadow sets are cloned, replacements rebuild strings, and substitution uses a fresh parser. | Not measured; unchanged. | Source inspection. |
| Container member lookup: `commands/symbol_render.rs:163,171` | Confirmed, deferred. Each member still scans the symbol table. This delivery removes ownership copies, not those comparisons. | Not measured; unchanged. | Zoom/container tests passed. |
| Post-import reads: `commands/add_import.rs:640`, `remove_import.rs:236`, `organize_imports.rs:224` | Confirmed, deferred. Final bytes are reread for LSP post-write even when formatting did not run. Reusing attempted bytes must account for rollback and formatter output. Backup protocol is outside this local optimization. | Not measured; unchanged. | Source inspection; import suite passed. |

## Verification

- Cargo: `cargo 1.99.0 (5f94df478 2026-08-27)`.
- Format: `rustfmt 1.10.0-stable (b940084d7e 2026-09-28)`;
  `cargo fmt --all -- --check` passed after restoring mutations.
- `cargo test -p agent-file-tools --lib parser::tests:: -- --nocapture`:
  135 passed, 1 intentionally ignored manual profile. The substring also matches
  bash-rewrite/patch parser tests.
- `cargo test -p agent-file-tools --lib calls::tests:: -- --nocapture`: 12 passed.
- `cargo test -p agent-file-tools --lib commands::zoom:: -- --nocapture`: 54 passed.
- `cargo test -p agent-file-tools --lib commands::read:: -- --nocapture`: 17 passed.
- `cargo test -p agent-file-tools --lib hashline::snapshot:: -- --nocapture`: 17 passed.
- `cargo test -p agent-file-tools --lib commands::ast_`: 18 passed.
- `cargo test -p agent-file-tools --lib imports::`: 135 passed.
- `cargo test -p agent-file-tools --lib indent::tests::`: 8 passed.
- `cargo test -p aft-tokenizer --lib -- --nocapture`: 3 passed.
- Windows cross-check was started, then cancelled with the parent's approval
  because the shared compile-slot queue was slow; the parent train owns that gate.
- A scoped AFT inspection was partial while rust-analyzer indexed. It is not
  reported as a clean diagnostic pass; compiled Rust unit targets provide type
  verification for the changed code.
- No config keys, tool arguments, ranking/routing files, package manifests or
  new response caps changed. GitHub read and outline were not edited.

## Mutation controls

The original #11 implementation failed its new counter test before optimization
(24,006,001 operations for 4,004 symbols). After implementation, nine controls
were applied together and each named test was run with its own exact-name filter.
Each invocation ran one test and failed only that named test; other tests were
filtered out. Controls restored the linear name scan, old BPE merge, owned table
copies, non-JS reparse, reverse sibling scan, zoom disk reparse, indentation leak,
legacy hashline render/reread, and removed the source identity fence.

Before mutation, the exact live files were staged and `git diff --stat` was empty.
During mutation it reported seven files, 12 insertions and 14 deletions. Restore
used `git checkout --` **against that staged live index**, followed by `touch`;
`git diff --stat` was empty again. All suites above passed on the restored code.
The machine-readable delivery includes the exact failing names and diff evidence.
