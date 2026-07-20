# Implement AFT benchmark canon Beads

You are a coding agent working in the AFT repository.

Repository path:

```text
D:\Coding\_tools\aft-src
```

Lexical canon package path already placed by the user:

```text
D:\Coding\_tools\aft-src\benchmarks\aft-lexical-canon-package
```

The lexical canon package has **not** yet been integrated into the repository. Treat that package as source material for implementation, not as already-wired benchmark data.

Your task is to implement the imported Beads epic:

```text
aft-t6p.bench.quick
```

Work sequentially, one Bead at a time. Do not spawn subagents for code edits. Subagents are allowed only for read-only code exploration, repository investigation, or debugging if your host supports them, but you remain responsible for final edits and validation.

---

## Operating rules

Use the local Beads CLI as source of truth.

Before editing any code:

```bat
cd /d D:\Coding\_tools\aft-src
bd show aft-t6p.bench.quick --json
bd dep cycles
bd ready --json
```

Then select the next ready child Bead under `aft-t6p.bench.quick`.

For every Bead:

1. Read it fully:

   ```bash
   bd show <bead-id> --json
   ```

2. Read relevant parent/blocker/related context.

3. Claim it before editing:

   ```bash
   bd update <bead-id> --claim --json
   ```

4. If claim fails, do not edit. Refresh `bd ready --json` and choose another ready Bead or leave a handoff.

5. Implement the smallest coherent change that satisfies the Bead.

6. Run focused validation for that Bead.

7. Commit after successful validation:

   ```bash
   git status --short
   git add <changed-files>
   git commit -m "<concise bead-aware commit message>"
   ```

8. Close the Bead only with evidence:

   ```bash
   bd close <bead-id> --reason "validated: <commands and result>; reachability: <entry point to implementation path>; handoff: <summary>" --json
   ```

If `bd close` syntax differs, inspect:

```bash
bd close --help
```

Do not guess unsupported Beads commands. In particular, do not use `bd claim`; use `bd update <id> --claim --json` unless local help proves a different installed syntax.

---

## Recovery if the Beads are missing

If `bd show aft-t6p.bench.quick --json` fails because the graph has not been imported, stop and report that the Beads graph is missing. Do **not** invent replacement Beads.

If the user provides or points you to the JSONL graph file, import it with the local `bd import` workflow, then run:

```bash
bd dep cycles
bd show aft-t6p.bench.quick --json
bd ready --json
```

---

## Implementation intent

This work upgrades `benchmarks/semble/pilot.ts` and related benchmark assets from an exploratory harness into a decision-grade benchmark package.

The concrete goals are:

1. Integrate the lexical canon package located at:

   ```text
   benchmarks\aft-lexical-canon-package
   ```

2. Stop deriving lexical/identifier ground truth from a runtime ripgrep pass.

3. Add suite-aware benchmark data and scoring.

4. Add AFT-native lexical/search modes beyond `aft-grep`, especially:
   - `fts5_search`;
   - `fts5_find_symbol` exact mode;
   - `fts5_find_symbol` prefix mode;
   - `glob`;
   - `ast_search`.

5. Keep search modes in separate benchmark suites rather than forcing all modes into one misleading leaderboard.

6. Fix benchmark aggregation so empty/error/unavailable attempts count as zero-scoring attempts instead of disappearing from averages.

7. Split latency into meaningful components where possible:
   - configure/init;
   - index update;
   - model load/status wait;
   - warm query;
   - candidate generation;
   - rerank;
   - end-to-end.

8. Add JSON/Markdown report outputs suitable for human review and CI/agent parsing.

9. Add validation scripts/tests so regressions are caught through the public benchmark entry point.

---

## Current repository context to inspect first

Start by reading these files and directories:

```text
benchmarks/semble/pilot.ts
benchmarks/semble/aft-ndjson.ts
benchmarks/semble/fixtures.json
benchmarks/semble/annotations/
benchmarks/aft-lexical-canon-package/
crates/aft/src/commands.rs
crates/aft/src/handlers/
crates/aft/src/fts5/
```

Do not assume the package has the final desired layout. Inspect it.

Pay attention to these likely AFT commands/modes:

```text
grep
glob
semantic_search
ast_search
fts5_index
fts5_search
fts5_find_symbol
fts5_read_symbol
status
configure
```

Confirm exact NDJSON parameter names in the current source before wiring wrappers.

---

## Critical benchmark correctness rules

### 1. No runtime oracle generation

Do not use `rgSearch()` or any runtime search mode to generate `allRelevant` for lexical/identifier scoring.

Runtime search modes may be benchmark competitors, but not the oracle.

Use checked-in canon files as the ground truth.

If the current lexical canon package contains `review_status: "seed"` rows, the benchmark runner may load them, but reports must clearly distinguish:

```text
review_status=seed
review_status=validated
review_status=rejected
```

Do not silently treat seed rows as final human-verified truth for hard CI gates unless the relevant Bead explicitly tells you to.

### 2. Every attempted mode/query must produce a row

A failed, empty, unavailable, or timed-out attempt must still produce a result row with metrics set to zero where appropriate.

Use a row shape similar to:

```ts
interface QueryAttempt {
  suite: string;
  mode: string;
  query_id: string;
  query: string;
  repo_name: string;
  attempted: true;
  status: "ok" | "empty" | "unavailable" | "error" | "timeout";
  error?: string;
  latency_ms: number;
  results: SearchResult[];
  recall_at_k: number;
  mrr: number;
  ndcg_at_k: number;
}
```

Aggregate over attempted rows, not only successful non-empty rows.

### 3. Keep suites separate

At minimum, support these suites:

```text
semantic_nl
identifier_exact
identifier_prefix
path_lookup
structural
```

Do not mix identifier and natural-language query rows into one aggregate.

A mode can appear in multiple suites, but each suite/mode pair must have its own aggregate.

### 4. Use mode eligibility

Each canon query may declare `eligible_modes`. Respect it.

For example, `ast_search` should not be scored against ordinary natural-language semantic queries unless a Bead explicitly asks for that experiment. `glob` should primarily be evaluated in a path lookup suite. `fts5_find_symbol_exact` should primarily be evaluated in exact symbol suites.

### 5. Preserve persistent process fairness

Use persistent `AftSession` processes for AFT-native modes where possible. Do not regress FTS5/AFT grep back to per-query process spawning.

If a mode cannot share a session safely, report that as a limitation and measure it separately.

### 6. Do not hide unavailable modes

If FTS5 is not available because the binary was not built with the right feature, emit `status: "unavailable"` rows and a clear preflight warning, or fail fast if the requested profile requires FTS5.

Do not silently omit the mode from the report.

---

## Suggested sequential Bead implementation order

Follow the actual Beads dependencies. If dependencies allow multiple ready Beads, prefer this order:

```text
aft-t6p.bench.quick.00
aft-t6p.bench.quick.01
aft-t6p.bench.quick.02
aft-t6p.bench.quick.03
aft-t6p.bench.quick.04
aft-t6p.bench.quick.05
aft-t6p.bench.quick.06
aft-t6p.bench.quick.07
aft-t6p.bench.quick.08
```

Expected intent of the sequence:

1. Lock benchmark scope/profile decisions.
2. Integrate the lexical canon package into the canonical benchmark path.
3. Refactor the runner around suite-aware attempts and strict denominators.
4. Add AFT-native lexical/search modes.
5. Add candidate-pool/hybrid/rerank correctness improvements.
6. Add latency/report schema improvements.
7. Add validation scripts/tests/documentation.
8. Run implementation verification and fix gaps.
9. Leave a clean handoff.

Use the Bead body as the authoritative contract if it differs from this summary.

---

## Lexical canon package integration guidance

The user placed the package here:

```text
benchmarks\aft-lexical-canon-package
```

Expected package contents may include:

```text
README.md
INTEGRATION_PROMPT.md
PACKAGE_SUMMARY.json
benchmarks/semble/canon/
  repos.json
  lexical-canon.schema.json
  mode-matrix.json
  identifier-exact.json
  identifier-prefix.json
  path-lookup.json
  structural.json
  unverified-seeds.json
  README.md
  validation-checklist.md
benchmarks/semble/tools/
  validate-lexical-canon.ts
```

Recommended integration target:

```text
benchmarks/semble/canon/
benchmarks/semble/tools/
```

Do not leave the package nested under `benchmarks/aft-lexical-canon-package` as the only source of benchmark truth. Either copy/move the canonical assets into `benchmarks/semble/canon/` and `benchmarks/semble/tools/`, or implement the path expected by the active Beads. Avoid duplicated active canon files unless there is a clear migration note.

If moving files, preserve provenance in docs and commit message.

---

## Runner design target

Prefer refactoring `pilot.ts` into modules if the Bead scope allows it:

```text
benchmarks/semble/pilot.ts              # CLI orchestration / compatibility entry point
benchmarks/semble/bench-runner.ts       # profile/suite loop
benchmarks/semble/bench-modes.ts        # rg, grep, fts5, symbol, glob, ast, semantic, hybrid, rerank
benchmarks/semble/bench-metrics.ts      # recall, mrr, ndcg, path/symbol/span matching
benchmarks/semble/bench-report.ts       # JSON and Markdown reports
benchmarks/semble/bench-profiles.ts     # smoke/quick/extended/full profiles
benchmarks/semble/canon/                # checked-in ground truth
benchmarks/semble/tools/                # canon validation and support scripts
```

If full modularization is too large for the current Bead, keep `pilot.ts` working and create a follow-up Bead for deeper refactoring. Do not perform a large rewrite without tests.

---

## CLI behavior target

Preserve existing common usage where practical:

```bash
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --k 10
```

Add or support these flags if required by the Beads:

```bash
--suite semantic-nl|identifier-exact|identifier-prefix|path-lookup|structural|all
--profile smoke|quick|extended|full
--backend model2vec,fastembed,semantic-api
--mode rg,aft-grep,fts5-search,fts5-find-symbol-exact,fts5-find-symbol-prefix,glob,ast-search,semantic,hybrid,rerank
--candidate-pool 50
--rerank-pool 50
--repetitions 3
--warmups 1
--allow-degrade false
--report-json pilot-report.json
--report-md pilot-report.md
```

Reject invalid modes and invalid suites early with a non-zero exit code.

If you add new flags, update help text and docs.

---

## Search mode implementation notes

### `fts5_search`

Use after a persistent session is configured and `fts5_index` has run.

Expected command shape, to verify in source:

```json
{
  "command": "fts5_search",
  "query": "<query>",
  "scope": "all",
  "top_k": 10
}
```

Do not assume `scope` is fully enforced unless current source confirms it. If `scope` is only reported but not used by the planner, document that limitation and avoid scoring fake submodes such as `fts5_search_symbols` unless implemented.

### `fts5_find_symbol`

Use for exact/prefix symbol lookup suites.

Expected command shape, to verify in source:

```json
{
  "command": "fts5_find_symbol",
  "name": "<symbol>",
  "mode": "exact",
  "top_k": 10
}
```

and:

```json
{
  "command": "fts5_find_symbol",
  "name": "<symbol prefix>",
  "mode": "prefix",
  "top_k": 10
}
```

Map returned `file_path`, `start_line`, `end_line`, `symbol_name`, and `symbol_kind` into the benchmark `SearchResult`.

### `glob`

Use for path lookup suite.

Expected command shape, to verify in source:

```json
{
  "command": "glob",
  "pattern": "<glob pattern>"
}
```

Do not compare glob to semantic search as if it were content search.

### `ast_search`

Use for structural suite.

Expected command shape, to verify in source:

```json
{
  "command": "ast_search",
  "pattern": "<ast-grep pattern>",
  "lang": "rust",
  "context": 0
}
```

Only run AST patterns where the canon entry declares an AST pattern and language.

### `grep`

Keep the existing AFT grep mode as `aft-grep`.

Do not use it as the oracle.

### `rg`

Keep external ripgrep as optional external baseline if useful, but do not make it required for canon scoring and do not use it to generate ground truth at runtime.

---

## Validation expectations

Use the narrowest validation first, then broader checks.

Likely commands:

```bash
bun run benchmarks/semble/tools/validate-lexical-canon.ts
bun run benchmarks/semble/pilot.ts --help
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --profile smoke --suite identifier-exact --k 10 --verbose
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --profile smoke --suite all --k 10 --report-json pilot-report.json --report-md pilot-report.md
```

If the binary does not exist, build or report the exact required build command from repository docs/source. Likely candidates:

```bash
cargo build --release --features semantic-fts5
```

or the feature set required by current branch.

Run available static checks/tests if reasonable:

```bash
bun --version
cargo check --features semantic-fts5
cargo test --features semantic-fts5
```

Do not claim validation that you did not run. If a command cannot run on the machine, state the failure precisely and create/leave a follow-up Bead if necessary.

---

## Implementation verification requirements

Before closing the final verification Bead, prove:

1. The lexical canon package has been integrated into active benchmark paths.
2. Runtime scoring no longer derives identifier relevance from `rgSearch()`.
3. Every attempted suite/mode/query emits a row, including empty/error/unavailable cases.
4. Aggregates use attempted-row denominators.
5. Natural-language, identifier, path, and structural suites are separated.
6. AFT-native modes beyond `aft-grep` are wired where the source supports them:
   - `fts5_search`;
   - `fts5_find_symbol` exact;
   - `fts5_find_symbol` prefix;
   - `glob`;
   - `ast_search`.
7. Reports include enough metadata for an agent or CI to parse:
   - schema version;
   - command/config;
   - repo/profile/suite/mode;
   - per-query attempts;
   - aggregate metrics;
   - failures/unavailable modes.
8. Existing semantic benchmark behavior still works.
9. Documentation explains limitations, especially seed vs validated canon rows.

---

## Discovered work policy

If you find side work:

- If it blocks current acceptance, create a blocker Bead and stop the current Bead.
- If it is useful but not blocking, create a discovered follow-up Bead under `aft-t6p.bench.quick` or the appropriate parent.
- If the solution is unknown and requires investigation, create a spike/investigation Bead instead of embedding unresolved research into an implementation Bead.

Do not hide discovered work in a final note only.

Suggested discovered-work examples:

```text
- FTS5 scope parameter is accepted but not actually enforced by planner.
- AST search result shape lacks enough path/span data for benchmark scoring.
- Canon seed row points to a stale path after pinned repo checkout.
- AFT session helper introduces 50ms polling quantization in latency measurements.
- Existing report schema is too unstable for CI regression gates.
```

---

## Commit policy

Make one commit per completed Bead where feasible.

Commit messages should reference the Bead ID:

```text
bench: integrate lexical canon fixtures (aft-t6p.bench.quick.01)

bench: add suite-aware attempt scoring (aft-t6p.bench.quick.02)

bench: add AFT-native lexical modes (aft-t6p.bench.quick.03)
```

After each commit, close or update the Bead with:

```text
files changed
validation run
reachability proof
remaining risks
next recommended Bead
```

---

## Final handoff expected

At the end of the epic, produce:

```markdown
## Benchmark canon implementation handoff

### Beads completed

### Commits

### Files changed

### Validation run

### Report examples generated

### Remaining seed/unverified canon rows

### Known limitations

### Follow-up Beads created

### Recommended next command
```

Do not mark the epic/milestone complete unless the implementation-verification Bead is satisfied.
