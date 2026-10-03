# Baseline-only regex fallback instability

Supplemental query: `search.*index`, shape `regex`, executed lane `fallback_walk`. Same before-binary, same frozen AFT corpus, repeated three times with no source changes. This lane is not modified by the performance train. It reports a budget-limited, capped result; wall-clock cuts and/or parallel enumeration are likely causes. No fix or engine-budget change is made.

The parent authorized continuing the required-lane checks after this baseline-only difference was established. Only health/repetition footers may be excluded; actual differing fallback rows remain reported, and the all-case comparator still returns failure for them.

## Repeat 0 versus 1

```diff
--- before-repeat-0
+++ before-repeat-1
@@ -84,7 +84,7 @@
 2545:         // The valid project key (search_index) still applies via partial-parse...
 ... and 9 more matches
 
-Found 101 match across 73 file (capped)
+Found 100 match across 72 file (capped)
 [interpreted_as: regex]
 
 shown 50 of ≥51 results (budget) · narrow: offset, topK, path, includeTests
```

## Repeat 1 versus 2

```diff
--- before-repeat-1
+++ before-repeat-2
@@ -54,6 +54,29 @@
 .alfonso/release-notes/v0.47.1.md
 34: - Symbol-cache freshness formally adopts stat-first trust (matching the search index): size+mtime match serves the cache; content hashing runs only when mtime moved; the file watcher remains the inval…
 
+.alfonso/release-notes/v0.47.2.md
+30: search), and the LSP diagnostics store is indexed by file so watcher invalidation no longer scans every entry.
+
+.alfonso/release-notes/v0.48.1.md
+11: - Cross-project `aft_search` on a repository that has no AFT index now returns a bounded lexical scan with a disclosure instead of failing with `not_indexed`. The error dead-ended agents into shell fa…
+25: - **Outline generation** (also used by `aft_zoom` container menus and top-ranked search snippets) replaced a quadratic insertion scan with an indexed one. Files with thousands of one-method containers…
+
+.alfonso/release-notes/v0.49.1.md
+13: **Grep footer fires only where it helps** (PR #176 by @iceteaSA). The "use aft_search" footer no longer appears for single named files, paths outside the indexed root, or files modified within the las…
+15: **Semantic search degradation names its reason** (#177). When query embedding times out on a slow provider, the fallback now says so — including the configured budget and the `semantic.query_timeout_m…
+
+.alfonso/release-notes/v0.49.4.md
+28: - Interactive search waits are bounded while background index builds run.
+
+.alfonso/release-notes/v0.50.1.md
+7: - **`aft_search` / `grep`: indexed case-insensitive regex search no longer loses Unicode case-folding matches.** The trigram index decomposed patterns without the case-insensitive flag, so matches lik…
+
+.alfonso/release-notes/v0.51.0.md
+24: - `worktree.ram_overlay` (default off): linked worktrees can apply their own edits to the in-RAM search index delta, so search reflects local changes without writing shared artifacts.
+
+.alfonso/release-notes/v0.52.0.md
+13: Cold builds (callgraph, search index, semantic embedding, and inspect deep scans) are now covered by a durable breaker: repeated process deaths attributed to the same build suspend that work instead o…
+
 crates/aft/src/commands/status.rs
 102:         let search_index_info = match self.search_index().try_read() {
 443:                 "search_index": config.indexes.trigram,
@@ -82,9 +105,8 @@
 2539:             tier("user", r#"{ "search_index": true }"#),
 2542:                 r#"{ "storage_dir": "/tmp/evil", "bash_permissions": true, "search_index": false }"#,
 2545:         // The valid project key (search_index) still applies via partial-parse...
-... and 9 more matches
 
-Found 100 match across 72 file (capped)
+Found 100 match across 78 file (capped)
 [interpreted_as: regex]
 
 shown 50 of ≥51 results (budget) · narrow: offset, topK, path, includeTests
```
