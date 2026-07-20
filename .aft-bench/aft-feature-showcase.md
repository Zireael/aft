AFT Feature Showcase
Generated 2026-06-24T19:57:10.734Z

Target
  Binary: ./target/release/aft/aft.exe
  Project: D:\Coding\_tools\aft-src
  Query: where is semantic search reranking handled
  Expected file: crates/aft/src/commands/semantic_search.rs
  Top K: 10

Baseline vs Retrieval Intelligence
  Mode                                ms     results  snips  tokens  expected  top file                                       
  ----------------------------------  -----  -------  -----  ------  --------  -----------------------------------------------
  AFT-GREP baseline                   310ms  2        0      0       -         \\?\D:\Coding\_tools\aft-src\docs\benchmarks.md
  AFT-FTS5 baseline                   126ms  3        3      436     -         benchmarks/semble/README.md                    
  RI v2 semantic_search               502ms  2        0      0       -         \\?\D:\Coding\_tools\aft-src\docs\benchmarks.md
  RI v2 token-budget semantic_search  559ms  2        0      0       -         \\?\D:\Coding\_tools\aft-src\docs\benchmarks.md

AFT-GREP baseline
  Command: grep
  Status: OK
  Latency: 310ms
  Results: 2
  Top file: \\?\D:\Coding\_tools\aft-src\docs\benchmarks.md
  Context: 0 snippets, 0 snippet tokens
  Notes: expected file not in top results; baseline comparison path

AFT-FTS5 baseline
  Command: fts5_search
  Status: OK
  Latency: 126ms
  Results: 3
  Top file: benchmarks/semble/README.md
  Speed: 2.46x faster than baseline
  Context: 3 snippets, 436 snippet tokens
  Notes: expected file not in top results; baseline comparison path

RI v2 semantic_search
  Command: semantic_search
  Status: OK
  Latency: 502ms
  Results: 2
  Top file: \\?\D:\Coding\_tools\aft-src\docs\benchmarks.md
  Speed: 0.62x faster than baseline
  SearchPlan: NaturalLanguage, safety lane FTS5Body, 5 lanes
  Context: 0 snippets, 0 snippet tokens
  Budget: total=4000, per_candidate=300, soft_overflow=0
  Notes: expected file not in top results; search plan emitted; semantic unavailable surfaced explicitly; lexical fallback surfaced explicitly

RI v2 token-budget semantic_search
  Command: semantic_search
  Status: OK
  Latency: 559ms
  Results: 2
  Top file: \\?\D:\Coding\_tools\aft-src\docs\benchmarks.md
  Speed: 0.55x faster than baseline
  SearchPlan: NaturalLanguage, safety lane FTS5Body, 5 lanes
  Context: 0 snippets, 0 snippet tokens
  Budget: total=4096, per_candidate=384, soft_overflow=128
  Notes: expected file not in top results; search plan emitted; token-budget context request enabled; semantic unavailable surfaced explicitly; lexical fallback surfaced explicitly

Feature Cards
  [available] SearchPlan
     Why it matters: Shows query intent, lane weights, safety lanes, and retrieval strategy instead of hiding search as a black box.
     Evidence: intent NaturalLanguage; 5 lane weights; safety lane FTS5Body
  [missing] Candidate provenance
     Why it matters: Explains which lanes contributed each result and whether graph/context/ranking features affected the final order.
     Evidence: No retrieval_intelligence_provenance field in semantic_search output.
  [degraded] Definition-aware ranking
     Why it matters: Moves likely definitions and symbol matches above generic mentions, which matters most for agent code navigation.
     Evidence: No ranking features reported for this query.
  [available] Diagnostics and context tools
     Why it matters: Turns search from a list of hits into explainable workflow primitives: orient, why-missed, impact, and context pack.
     Evidence: FTS5 index update: ok; FTS5 doctor: ok; FTS5 symbol lookup: ok; FTS5 read symbol: ok; Semantic doctor: ok; Explain search: ok; Why missed: ok; Orient: ok; Impact delta: ok; Context pack: ok
  [available] Token-budget context
     Why it matters: Shows the branch's context-volume improvement separately from ranking quality: more selected snippets can feed reranking and agent context without changing base recall.
     Evidence: 0 snippets selected; 0 snippet tokens; budget total=4096, per_candidate=384, soft_overflow=128
  [available] Telemetry privacy posture
     Why it matters: Runtime search writes operational telemetry while defaulting to hashed queries, giving performance insight without raw-query storage by default.
     Evidence: telemetry database created at C:\Users\zir\AppData\Local\Temp\aft-feature-showcase-aft-src\ri\aft.db

Workflow Diagnostics
  OK FTS5 index update (558ms)
     FTS5 update: processed=1579 added=0 updated=0 removed=0 symbols=17583
     Why it matters: Builds the SQLite FTS5 symbol/body/path index used by exact lookup, prefix lookup, full-text search, and hybrid retrieval.
  OK FTS5 doctor (3810ms)
     FTS5 Doctor
     Why it matters: Confirms whether FTS5 is compiled, enabled, populated, and healthy before judging search quality.
  OK FTS5 symbol lookup (61ms)
     1 symbol candidates; top handle_semantic_search in ?
     Why it matters: Shows exact symbol lookup over the FTS5 symbol table, the clearest win over plain grep for code navigation.
  OK FTS5 read symbol (59ms)
     FTS5 Read Symbol: "handle_semantic_search"  crates/aft/src/commands/semantic_search.rs:142-239
     Why it matters: Reads canonical source for a symbol from the index, turning lookup results into usable code context.
  OK Semantic doctor (61ms)
     semantic: disabled | disabled | 0 queries, p50=0ms | 1 suggestions
     Why it matters: Reports semantic backend, index, and metrics health so quality issues can be separated from provider/config problems.
  OK Explain search (499ms)
     intent NaturalLanguage, 5 lane weights, safety lane FTS5Body
     Why it matters: Explains lane weights and safety lanes, so users know why the search behaved the way it did.
  OK Why missed (563ms)
     not in candidate pool; 5 lanes reported miss details
     Why it matters: Shows whether an expected file entered the candidate pool and which lanes missed it.
  OK Orient (558ms)
     2 primary files, 8 entry symbols
     Why it matters: Turns search hits into an entry-point map instead of a flat list.
  OK Impact delta (1935ms)
     graph healthy, blast radius 27, mutation risk Medium
     Why it matters: Estimates blast radius and mutation risk for a symbol-level change.
  OK Context pack (559ms)
     2 items, 2098/4000 tokens used
     Why it matters: Packages relevant code into a bounded context budget for agent workflows.

Recommendations
  - RI v2 did not place the expected file in the top K for this query; try a more specific symbol/query or run against a cleaner project root before using this as quality evidence.
  - AFT-FTS5 baseline was fastest in this run; use speed together with rank quality, not by itself.
  - Diagnostics, orientation, impact, and context-pack commands are suitable for workflow demos on this project.
