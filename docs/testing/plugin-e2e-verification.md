# Plugin e2e repair verification

Baseline: `14533e309dfd75b6c9757849fca22008c0d4d44d`; macOS arm64, Bun 1.4.2.

All suite runs were serial, after `cargo build -p agent-file-tools --bin aft` completed (103 s). No product code changed.

## Baseline failure inventory

Every unique failing test below is copied from the baseline logs. The first run supplied the CI-pinned binary without a wire-version declaration, as the existing CI does. The second supplied `SUBC_CORE_WIRE_VERSION=2` to actually exercise subc. Repeated summaries are deduplicated.

### baseline

**opencode-plugin** (219.05 s)

- e2e semantic search tool > aft_search degrades to a lexical fallback when semantic is disabled
- glob/conflicts tool_call e2e > conflicts reports line-numbered merge conflict regions
- e2e bash command (OpenCode adapter + bridge + Rust) > rewrites grep -r to grep tool with enforced code-search footer
- e2e bg notifications (OpenCode adapter + bridge + Rust) > detach response leaves an earlier completion queued for the next tool result
- e2e bg notifications (OpenCode adapter + bridge + Rust) > auto-promotion response leaves an earlier completion queued for the next tool result

**pi-plugin** (37.89 s)

- e2e bash command (Pi adapter + bridge + Rust) > rewrites grep -r to grep tool with footer hint when enabled

**aft-bridge** (0.24 s)

- No failures (the initial bridge run skipped 15 subc cases; not a valid green).

### wire2

**opencode-plugin** (846.40 s)

- e2e semantic search tool > aft_search degrades to a lexical fallback when semantic is disabled
- e2e bash command (OpenCode adapter + bridge + Rust) > rewrites grep -r to grep tool with enforced code-search footer
- subc transport parity sweep > subc parity: edit/write tool_call > edit append mutates the file and returns server text
- subc transport parity sweep > subc parity: edit/write tool_call > edit oldString/newString mutates the file, returns text, and preserves UI filediff
- subc transport parity sweep > subc parity: edit/write tool_call > edit on a >512KB file returns the preview hunk as a patch filediff
- subc transport parity sweep > subc parity: edit/write tool_call > edit symbol+content mutates the symbol and returns server text
- subc transport parity sweep > subc parity: edit/write tool_call > edit edits[] batch mutates all edits and returns server text
- subc transport parity sweep > subc parity: edit/write tool_call > write creates and overwrites files through tool_call
- subc transport parity sweep > subc parity: edit/write tool_call > write on a >512KB file returns the preview hunk as a patch filediff
- subc transport parity sweep > subc parity: edit/write tool_call > denied preview approval returns permission_denied and leaves the file unchanged
- subc transport parity sweep > subc parity: edit/write tool_call > edit oldString not found throws before mutating
- subc transport parity sweep > subc parity: read-only spine tool_call > grep returns server-rendered matches through tool_call
- subc transport parity sweep > subc parity: read-only spine tool_call > grep appends the plugin-side skipped path footer after tool_call
- subc transport parity sweep > subc parity: read-only spine tool_call > aft_search returns server-rendered literal search text through tool_call
- subc transport parity sweep > subc parity: read-only spine tool_call > aft_inspect returns server-rendered todos details through tool_call
- subc transport parity sweep > subc parity: read-only spine tool_call > aft_outline returns single-file Text output through tool_call
- subc transport parity sweep > subc parity: read-only spine tool_call > aft_outline returns plain-text directory outline through tool_call
- subc transport parity sweep > subc parity: read-only spine tool_call > aft_outline files:true returns the server-rendered files tree through tool_call
- subc transport parity sweep > subc parity: read-only spine tool_call > aft_outline returns multi-file Text output for array targets through tool_call
- subc transport parity sweep > subc parity: read-only spine tool_call > aft_outline includeTests controls directory test-file visibility through tool_call
- subc transport parity sweep > subc parity: zoom tool_call > single-symbol zoom returns server-rendered text through tool_call
- subc transport parity sweep > subc parity: zoom tool_call > same-file multi-symbol partial failure returns Incomplete text
- subc transport parity sweep > subc parity: zoom tool_call > url zoom returns server-rendered text through tool_call
- subc transport parity sweep > subc parity: zoom tool_call > large containers render the member-signature menu
- subc transport parity sweep > subc parity: zoom tool_call > single-symbol not-found errors still throw
- subc transport parity sweep > subc parity: zoom tool_call > cross-file targets return all-success server-rendered text through tool_call
- subc transport parity sweep > subc parity: zoom tool_call > cross-file targets return incomplete text for partial failures
- subc transport parity sweep > subc parity: zoom tool_call > cross-file targets can include callgraph annotations
- subc transport parity sweep > subc parity: callgraph tool_call > callers returns server-rendered hits through tool_call
- subc transport parity sweep > subc parity: callgraph tool_call > call_tree returns forward calls through tool_call
- subc transport parity sweep > subc parity: callgraph tool_call > trace_to_symbol returns a path through tool_call
- subc transport parity sweep > subc parity: callgraph tool_call > symbol_not_found is returned as plain text instead of thrown
- subc transport parity sweep > subc parity: callgraph tool_call > genuine argument errors throw
- subc transport parity sweep > subc parity: honest reporting > aft_outline directory mode returns complete true below walk cap
- subc transport parity sweep > subc parity: honest reporting > aft_outline directory mode returns complete false when Rust walk truncates
- subc transport parity sweep > subc parity: honest reporting > aft_outline single file target keeps text output behavior
- subc transport parity sweep > subc parity: honest reporting > aft_outline array target keeps multi-file text output behavior
- subc transport parity sweep > subc parity: honest reporting > ast_grep_search reports no_files_matched_scope separately from zero hits
- subc transport parity sweep > subc parity: honest reporting > ast_grep_search reports searched-zero-hit result for valid scope
- subc transport parity sweep > subc parity: honest reporting > aft_import remove reports successful no-op when import is absent
- subc transport parity sweep > subc parity: apply_patch rollback > successful add+update patch changes disk and returns server summary
- subc transport parity sweep > subc parity: apply_patch rollback > preview then denied edit permission leaves disk unchanged
- subc transport parity sweep > subc parity: apply_patch rollback > total failure after preview throws the server error
- subc transport parity sweep > subc parity: apply_patch rollback > partial failure returns summary and keeps successful hunks
- subc transport parity sweep > subc parity: apply_patch rollback > external-directory and edit permissions are both requested
- subc transport parity sweep > subc parity: format_on_edit apply_patch > add hunk triggers formatter
- subc transport parity sweep > subc parity: format_on_edit apply_patch > update hunk triggers formatter
- subc transport parity sweep > subc parity: format_on_edit apply_patch > multi-file Add+Update both format
- subc transport parity sweep > subc parity: format_on_edit apply_patch > move hunk formats destination
- subc transport parity sweep > subc parity: format_on_edit apply_patch > delete hunk does NOT trigger formatter
- subc transport parity sweep > subc parity: format_on_edit apply_patch > mixed-language patch
- subc transport parity sweep > subc parity: format_on_edit apply_patch > patch with formatter excluded path
- subc transport parity sweep > subc parity: format_on_edit apply_patch > patch with formatter timeout succeeds with the unformatted patch
- subc transport parity sweep > subc parity: format_on_edit apply_patch > patch with formatter generic error
- subc transport parity sweep > subc parity: format_on_edit apply_patch > patch preview failure does not format or apply earlier hunks
- subc transport parity sweep > subc parity: format_on_edit apply_patch > patch where ALL hunks fail does NOT format anything
- subc transport parity sweep > subc parity: format_on_edit apply_patch > format_on_edit=false config
- subc transport parity sweep > subc parity: safety/undo > creates and restores a checkpoint
- subc transport parity sweep > subc parity: safety/undo > undo reverts an edit
- subc transport parity sweep > subc parity: safety/undo > history lists prior snapshots
- subc transport parity sweep > subc parity: safety/undo > multiple undos walk back the stack
- subc transport parity sweep > subc parity: safety/undo > list_checkpoints returns created checkpoints
- subc transport parity sweep > subc parity: safety/undo > aft_safety returns readable sections for checkpoint, list, restore, undo, and history
- subc transport parity sweep > subc parity: safety/undo > aft_safety asks external-directory and edit permissions for undo and restore
- subc transport parity sweep > subc parity: safety/undo > operation undo restores every file from a multi-file delete in one call
- subc transport parity sweep > subc parity: safety/undo > operation undo restores a recursive directory delete in one call
- subc transport parity sweep > subc parity: safety/undo > recursive delete rejects symlinks before touching the filesystem
- subc transport parity sweep > hashline edit patch survives registration and ndjson dispatch
- subc transport parity sweep > hashline edit patch survives registration and subc dispatch
- subc transport parity sweep > edit registration and Rust argument enforcement agree across transports
- subc transport parity sweep > server-rendered text matches NDJSON for representative tool calls
- e2e bg notifications (OpenCode adapter + bridge + Rust) > detach response leaves an earlier completion queued for the next tool result
- e2e bg notifications (OpenCode adapter + bridge + Rust) > auto-promotion response leaves an earlier completion queued for the next tool result

**pi-plugin** (64.98 s)

- e2e bash command (Pi adapter + bridge + Rust) > rewrites grep -r to grep tool with footer hint when enabled

**aft-bridge** (280.42 s)

- subc transport e2e lane > (unnamed)
- subc transport lifecycle e2e lane > module respawn mid-session reopens the route and recovers bg_events
- subc transport lifecycle e2e lane > two client records retain matched bg_events routing when the newest closes
- subc transport lifecycle e2e lane > multi-root sessions on one daemon stay isolated
- subc transport lifecycle e2e lane > first binds survive cold-configure contention
- subc transport lifecycle e2e lane > daemon restart on the same connection file recovers the existing pool
- subc rig daemon does not outlive its runner > a runner that exits without cleanup still takes its daemon down

### fixed1

**opencode-plugin** (743.32 s)

- glob/conflicts tool_call e2e > conflicts reports line-numbered merge conflict regions
- e2e search parity > grep parity: skips binary files
- e2e format_on_edit skip reasons > honest reporting: success=true still carries top-level format_skipped_reason
- e2e aft_zoom tool_call cutover > large containers render the member-signature menu
- e2e bash command (OpenCode adapter + bridge + Rust) > multiple bash permission asks are grouped before bash runs
- subc transport parity sweep > subc parity: read-only spine tool_call > aft_search returns server-rendered literal search text through tool_call
- subc transport parity sweep > hashline edit patch survives registration and ndjson dispatch
- subc transport parity sweep > hashline edit patch survives registration and subc dispatch
- subc transport parity sweep > edit registration and Rust argument enforcement agree across transports
- subc transport parity sweep > server-rendered text matches NDJSON for representative tool calls

**pi-plugin** (53.36 s)

- aft_conflicts (real bridge) > (unnamed)

**aft-bridge** (66.30 s)

- No failures (the initial bridge run skipped 15 subc cases; not a valid green).

## Causes and changes

- **Rig release incompatibility:** the pinned ck-subc 0.3.0 authenticates AFT but rejects ModuleHello: `invalid_hello (malformed HELLO body: missing field scheduled_tasks at line 1 column 57868)`. The full parity sweep's 5 s timeouts, ECONNREFUSED cascades, bridge catalog failures and notification setup failures stemmed from this. The live supervisor self-reports 0.20.39 (`ck` and `ck-subc`). Published 0.17.48 and 0.14.0 both passed the isolated `multi-root sessions on one daemon stay isolated` probe. 0.14.0 is the first published release after 0.3.0; pin that oldest compatible candidate, with GitHub asset SHA-256 digests for Darwin arm64 and Linux x64. Published assets now use zip, so extraction changes accordingly. No product/protocol bug remains demonstrated.
- **Silent skip:** existing Linux/macOS unit jobs supplied `SUBC_CORE_BIN` without `SUBC_CORE_WIRE_VERSION`, which skips bridge lanes. Both now declare wire v2, as do the new e2e jobs. Explicitly requested unusable rigs and CI setup failures throw rather than silently skipping. Two new preflight tests cover a missing executable and a missing wire declaration. Timeout errors now retain daemon stderr.
- **Subc configuration mismatch:** the rig globally disabled trigram search, unlike standalone. After attachment was repaired, `aft_search returns server-rendered literal search text through tool_call` failed `no_search_lanes_enabled`, and the representative grep parity case compared indexed output against `[index: fallback]`. Enable trigram in the rig and keep semantic disabled. Explicit per-test project index disables still work. Enable all tools at the rig user tier, consistent with the standalone harness fix.
- **`subc-parity.e2e.test.ts`:** both hashline registration/dispatch cases and the cross-transport registration-enforcement case supplied unresolved configs missing `disabled_tools`. Supply `[]`, preserving their existing assertions. This follows the feature-config contract introduced by `8dc373a99` and registration's fail-closed resolved-config requirement. Representative text parity also waits through the new `[index: building]` transient marker, without normalizing away final output differences.
- **OpenCode `semantic-search.test.ts`:** `aft_search degrades to a lexical fallback when semantic is disabled` expected an old raw reason sentence. The read-only spine cutover (`62960feb5`) uses Rust tool_call rendering (introduced by `3452ee5b2` and parity updates `568480a64`). Assert actual fixture lexical matches and `Search status: partial/incomplete.` instead; this strengthens the old test, which did not assert any returned match.
- **OpenCode and Pi `bash.test.ts`:** the grep rewrite tests now expect the `aft_search` footer. `947aa2d0d` introduced Rust-side steering and `8dc373a99` replaced the legacy registration flag with the resolved enabled tool surface. The default surface includes aft_search. The grep rewrite and enforcement assertions remain.
- **Timeouts:** conflicts fixture setup intermittently exceeded Bun's 5 s default (both plugins). During a slower run additional unrelated startup/fixture tests timed out: binary-file grep parity, format-skipped metadata, zoom member menu, and grouped permission asks. Use a 30 s integration-runner budget in CI; explicit per-test timing/deadline assertions are unchanged. The reported import formatting failure did not reproduce.

## CI and final verification

The reusable unit workflow has blocking Linux e2e jobs for OpenCode standalone, OpenCode subc parity, Pi, and aft-bridge. Each builds before running tests and fetches the pinned rig. OpenCode exceeded 10 minutes during the diagnostic run (743 s), so standalone and parity are separate jobs. The standalone job excludes only the parity file; the parity job selects its describe name, including reused suites. Bridge `test:unit` now excludes e2e to avoid duplicating its real-daemon suite in the unit jobs. Linux installs ripgrep for search parity.

Command used for each full local pass, from each package directory:

`AFT_BINARY_PATH=<worktree>/target/debug/aft SUBC_CORE_BIN=<fetched-pin> SUBC_CORE_WIRE_VERSION=2 bun test --timeout 30000 src/__tests__/e2e/`

| Suite | First green | Second consecutive green | Inventory |
| --- | ---: | ---: | --- |
| OpenCode | 445.96 s | 392.68 s | 328 pass, 1 skip, 0 fail |
| Pi | 89.56 s | 64.04 s | 111 pass, 0 fail |
| aft-bridge | 66.80 s | 63.88 s | 15 pass, 0 fail |

The only skip is the pre-existing optional ruff formatting case (ruff unavailable/too old locally); no subc cases skip. After these runs only formatting and this report changed. All three package typechecks passed. The new preflight tests passed before and after a fail-loud guard mutation; neutralizing the guard made exactly `subc e2e preflight > an explicitly requested missing daemon fails rather than skipping` fail (exit 0 instead of 1). The mutant was restored. `actionlint` and fetch-script shell syntax validation passed. Installation after the package-script edit used `bun install --frozen-lockfile` with no lockfile changes.

No product code was modified. No confirmed product bug remains; the initial protocol rejection was the obsolete test daemon pin, not current production incompatibility.
