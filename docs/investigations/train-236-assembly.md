# Train 236 assembly

Base: local main `8882535fcdf592877d6d9097f7f82526db2760eb` (origin/main train 235 plus provenance). Only explicitly requested delivery commits were cherry-picked; no worker branch ranges or provenance merges were imported.

## Conflict resolutions

- `840f9a0d3`, `crates/aft/src/commands/semantic_search/anchored_lane.rs`: retained bounded corpus admission and its rejected-file reporting rather than restoring the older direct `read_to_string`. Added regex compilation instrumentation; file reads are counted at the shared corpus reader, not twice in the caller.
- `840f9a0d3`, `crates/aft/src/search_index.rs`: retained the bounded read protecting against files growing after stat, and added file-read, candidate-sort and score-evaluation counters.
- `5bde301f5`, `crates/aft/src/commands/semantic_search/anchored_lane.rs` (two regions): combined per-query compiled matchers with the admitted-file reader and `Result`-based exclusion counts. The empty-candidate fast path returns the admission-report tuple.
- `704684c18`, `crates/aft/src/semantic_index.rs` (four regions): retained live shared-base plus delta iteration in reuse-map construction; retained base tombstones while moving removed private entries; combined shared/delta search iteration with per-shared-file eligibility caching. Shared seed payloads are immutable and copied only when requested for reuse; private delta payloads are moved during removal. Capacity calculation includes live shared-base metadata, not just private metadata.

The semantic overlay and readiness deliveries merged without textual conflicts. Resident adoption still takes only the immutable donor seed, verifies the borrower's files before publication, and masks changed/deleted paths. Borrowed search loads retain their background flight across caller budgets. Warm search reloads use the independent four-permit pool, releasing their permit before a cold rebuild queues.

The read-latency and zoom/parser/patch/cache deliveries merged without textual conflicts. Rust formatting was normalized in `lib.rs`, `search_hot_path_measurements.rs`, and `semantic_index.rs`. Before the fifth delivery was requested, the full lib suite passed (4,027 passed, 47 ignored), the Windows tests cross-check passed, and lint passed; the final gates below cover all five deliveries.
