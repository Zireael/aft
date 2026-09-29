# Train 236 assembly

Base: local main `8882535fcdf592877d6d9097f7f82526db2760eb` (origin/main train 235 plus provenance). Only explicitly requested delivery commits were cherry-picked; no worker branch ranges or provenance merges were imported.

## Conflict resolutions

- `840f9a0d3`, `crates/aft/src/commands/semantic_search/anchored_lane.rs`: retained bounded corpus admission and its rejected-file reporting rather than restoring the older direct `read_to_string`. Added regex compilation instrumentation; file reads are counted at the shared corpus reader, not twice in the caller.
- `840f9a0d3`, `crates/aft/src/search_index.rs`: retained the bounded read protecting against files growing after stat, and added file-read, candidate-sort and score-evaluation counters.
- `5bde301f5`, `crates/aft/src/commands/semantic_search/anchored_lane.rs` (two regions): combined per-query compiled matchers with the admitted-file reader and `Result`-based exclusion counts. The empty-candidate fast path returns the admission-report tuple.
- `704684c18`, `crates/aft/src/semantic_index.rs` (four regions): retained live shared-base plus delta iteration in reuse-map construction; retained base tombstones while moving removed private entries; combined shared/delta search iteration with per-shared-file eligibility caching. Shared seed payloads are immutable and copied only when requested for reuse; private delta payloads are moved during removal. Capacity calculation includes live shared-base metadata, not just private metadata.

The semantic overlay and readiness deliveries merged without textual conflicts. Resident adoption still takes only the immutable donor seed, verifies the borrower's files before publication, and masks changed/deleted paths. Borrowed search loads retain their background flight across caller budgets. Warm search reloads use the independent four-permit pool, releasing their permit before a cold rebuild queues.

The read-latency and zoom/parser/patch/cache deliveries merged without textual conflicts. Rust formatting was normalized in `lib.rs`, `search_hot_path_measurements.rs`, and `semantic_index.rs`. Before the fifth delivery was requested, the full lib suite passed (4,027 passed, 47 ignored), the Windows tests cross-check passed, and lint passed; the final gates below cover all five deliveries.

## Verification

All Rust builds/tests used `CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER=`. Deliveries were tested in order before the next was picked. Cargo and TypeScript suites were not run concurrently.

- Semantic delivery: lib `semantic` (330 passed), `config_live` (32), `revision_` (14), `configure` (175), `runtime_drain` (75); integration `semantic` (53).
- Readiness delivery: `search_b2_readiness` (13), integration `aft_search_contract_test` (47), integration `standalone_search_deferred_test` (9), lib `semantic_search` (126), lib `configure` (177). Fresh debug binary plus bridge build: semantic-search TypeScript e2e (4).
- Search performance delivery: lib `commands::semantic_search` (133), `search_index` (91), `semantic_index` (140); standalone engine anchored (15), exact (15), fallback (8). All three opt-in hot-path counter tests passed; the real-corpus test required `AFT_PERF_CORPUS="$PWD"`. Python hot-path harness unit tests: 4 passed.
- Read latency: requested three TypeScript files (16 passed); OpenCode plugin unit suite (1,161 passed, 2 platform skips); plugin typecheck (`tsc --noEmit && tsc -p tsconfig.scripts.json`) passed.
- Zoom/parser/cache delivery: lib `commands::zoom` (52), `commands::outline` (25), `parser::tests` (120), `patch::` (61), `inspect::` (225), `lsp::` (172); integration `zoom` (71), `outline` (93).
- Final five-delivery branch: `cargo fmt --check`, `bun run lint`, Windows `cargo check -p agent-file-tools --tests --target x86_64-pc-windows-gnu`, fresh debug binary build, and full lib suite all passed. Full lib: **4,039 passed, 47 ignored, 0 failed**. Plugin unit suite rerun against that binary: **1,161 passed, 2 platform skips, 0 failed**. No baseline failure comparison was needed.

Verification corrections: the initial engine commands mistakenly filtered the integration umbrella and selected zero tests; the named standalone targets above were then run. An opt-in real-corpus counter run initially lacked its required environment variable and passed after supplying it. The initial formatting check found formatting inherited from the picked commits and the semantic conflict resolution; `cargo fmt` corrected it and the final check passed. The quality harness initially reported an unprovisioned ripgrep corpus; corpora were provisioned before retrying. `bun install --frozen-lockfile` completed without manifest or lockfile changes. Native linking emitted only the macOS compact-unwind size warning.
