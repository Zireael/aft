# Integration prompt for coding agent

You are integrating the AFT Semble lexical canon package.

Goals:
1. Add the files under `benchmarks/semble/canon/`.
2. Refactor `benchmarks/semble/pilot.ts` so lexical/path/structural suites load checked-in canon files instead of using a benchmark-time `rgSearch()` pass as ground truth.
3. Add AFT-native benchmark modes:
   - `fts5_find_symbol_exact`
   - `fts5_find_symbol_prefix`
   - `glob`
   - `ast_search`
4. Keep `rg` only as a baseline contestant, not as an oracle.
5. Emit one attempt row per `(suite, query, mode)`, including empty/error/unavailable attempts.
6. Aggregate by `(suite, mode)`.
7. Keep semantic NL, identifier exact, identifier prefix, path lookup, and structural results in separate tables.

Hard constraints:
- Do not derive `allRelevant` with ripgrep during benchmark runtime.
- Do not mix identifier/path/structural/NL suites into one aggregate leaderboard.
- Do not silently skip unavailable modes unless `--allow-degrade` is set and the JSON report records the degradation.
- Do not score `unverified-seeds.json` until those rows are pinned and reviewed.

Suggested implementation order:
1. Add canon loader + schema validation.
2. Add attempt-row result type.
3. Fix denominator accounting.
4. Add `fts5_find_symbol` wrappers.
5. Add `glob` wrapper.
6. Add `ast_search` wrapper.
7. Wire suite-specific mode matrix.
8. Add Markdown/JSON report sections per suite.
9. Add smoke command and one CI-safe validation command.
