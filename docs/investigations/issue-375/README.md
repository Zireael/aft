# Issue 375: config validation and slow read pre-phase

Investigation only; no production behavior changed. Read issue #375 and both comments, including the head-to-head table. Base: local main `48f5ec30b49f9136b25095e26f276cd6abd711a6`. Release tag: `b206689dddaece10e60e044da94699eb9ec352a9` (`v0.58.0`).

## Result

**The proposed unknown-key → indexes-off chain did not reproduce.** The released npm plugin plus released Linux x64 binary preserve the index settings, build both indexes, and find the filename. The same is true on macOS arm64 and current main. Neither resolver drops the whole document for an ordinary unknown top-level key. Moreover, the 0.58 defaults are **on**, not off.

**There is a separate, reproducible read pre-phase latency mechanism:** every ordinary text read awaits the host's session history, then its provider/model catalog, to determine image capability before dispatching the Rust read. Injecting 3 seconds in just `session.messages` produces `pre=3003ms`. This path does not load config. It is a strong candidate for the report's read-only, concurrent-call stalls, but the issue's aggregate timings alone cannot prove which await was slow on that user's host.

Do not tell the reporter that removing the unknown key is a proven search fix. We still need their raw tool response, resolved project/harness config, and the running bridge's identity/status, not only CLI/cache versions or an agent's summary.

## Reproduction and controls

The checked-in `plugin-probe.ts` calls the **real plugin's exported initialization function and registered tools**, with a minimal host SDK double (immediate allow permissions, session directory, messages, provider catalog, notifications). The actual engine is a standalone NDJSON child, no subc. The corpus is a separate `git init` repository with 240 TypeScript files, each containing an `addToCartN` function and cart-pricing doc comment. `lsp.auto_install:false` avoids unrelated downloads; ONNX and model downloads remain real. HOME, every XDG directory and TMPDIR are isolated. The npm bundle is extracted unchanged; its external `effect` dependency comes from the installed workspace. This is a plugin e2e harness, **not** a full OpenCode 1.18.32/Node 26 session; Bun 1.4.2 runs it.

User input:

```json
{"indexes":{"trigram":true,"semantic":true},"unknown_probe_key":true,"lsp":{"auto_install":false}}
```

| Run | Observed outcome |
| --- | --- |
| Released `@cortexkit/aft-opencode@0.58.0` + released `aft-linux-x64`, Linux amd64 Docker under OrbStack | Validation warning; search index ready for 240 files; semantic index ready for 480 chunks; warm search has no degraded/partial status. |
| Same npm bundle + released `aft-darwin-arm64` | Same; both indexes build despite validation warning. |
| Current main plugin source + freshly built main debug engine, macOS arm64 | `Unrecognized key: "unknown_probe_key"`; search ready, semantic building/embedding; filename found. |
| Release, key removed (valid config control) | Search results still returned, semantic building; no validation warning. |
| Release, both indexes explicitly false, unknown key retained | Every search **throws** `no_search_lanes_enabled`, not a successful 200-file walk. |
| Tag and main source loaders, false flags + unknown key | Both retain `trigram:false`, `semantic:false`, `callgraph:true`, and `lsp.auto_install:false`. This distinguishes partial preservation from dropping the document and reapplying on-defaults. |

Selected actual Linux release output (2026-09-29):

```text
07:28:45.062 WARN Config validation error in .../aft.jsonc: : Invalid input
07:28:45.588 INFO Binary version: 0.58.0
07:28:45.717 INFO index_event kind=build_ready plane=search ... elapsed_ms=118 files=240 trigrams=650
07:28:50.391 INFO index_event kind=build_started plane=semantic ...
07:29:01.194 INFO index_event kind=build_ready plane=semantic ... elapsed_ms=10802 files=240 chunks=480 skipped_rows=0
07:29:06.639 INFO perf tool=aft_search total=109ms pre=0ms bridge=109ms post=0ms
```

The npm bundle's warning text was `Invalid input`; the source loader on main emitted the report's more specific `Unrecognized key`. Do not conflate these strings, though both took the partial-parse path. The source loader extracted from the release tag returned:

```json
{"indexes":{"trigram":true,"semantic":true,"callgraph":true},"lsp":{"auto_install":false},"disabled_tools":["aft_delete","aft_move"]}
```

Its configure payload still contained the **original document**, including `unknown_probe_key`, in a user tier. The engine therefore independently encounters that key; it is not dependent on Zod's sanitized output.

Cold Linux results:
- `addToCart`: 10 ranked results, `shown 10 of ≥240 results (cap)`, semantic building, partial/incomplete, **not fully degraded**.
- `how does cart pricing work`: 10 lexical matches, `shown 10 of ≥200 results (cap)`, semantic building. This 200 is a result lower bound, not a 200-file walk cap.
- `cart239.ts`: first result `cart239.ts:2 [exact]`; ONNX download waiting disclosure; `shown 10 of 51 results (walk)`.
- `^export`: regex interpretation, 10 displayed results, budget trailer; no fully-degraded status.
- Warm `addToCart`: `shown 10 of ≥390 results (cap)`; neither building nor partial/incomplete status.

An initial exploratory run accidentally used a nested directory without its own `.git`, so the engine correctly treated it as a borrowed worktree index and reported `search_lanes_unavailable` because no shared snapshot existed in the isolated storage. The recorded successful matrix uses independent `git init` roots. This also demonstrates why root/ownership status matters; it is not evidence that unknown keys disable indexing.

### Repeat locally

Run from the repository root. Install with `bun install --frozen-lockfile`; build bridge imports with `bun run --cwd packages/aft-bridge build`. Fetch the assets with:

```sh
mkdir -p .tmp-375/release
gh release download v0.58.0 --pattern aft-darwin-arm64 --pattern aft-linux-x64 --pattern checksums.sha256 --dir .tmp-375/release
npm pack @cortexkit/aft-opencode@0.58.0 --pack-destination .tmp-375/release --silent
tar -xzf .tmp-375/release/cortexkit-aft-opencode-0.58.0.tgz -C .tmp-375/release
chmod +x .tmp-375/release/aft-*
ln -s "$PWD/packages/opencode-plugin/node_modules" .tmp-375/release/package/node_modules
```

Then, in a subshell so the operator's environment is unchanged:

```sh
(
  export HOME="$PWD/.tmp-375/home-repeat"
  export XDG_CONFIG_HOME="$HOME/config" XDG_CACHE_HOME="$HOME/cache"
  export XDG_DATA_HOME="$HOME/data" XDG_STATE_HOME="$HOME/state"
  export XDG_RUNTIME_DIR="$HOME/runtime" TMPDIR="$HOME/tmp"
  mkdir -p "$XDG_CONFIG_HOME/cortexkit" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR" "$TMPDIR"
  printf '%s\n' '{"indexes":{"trigram":true,"semantic":true},"unknown_probe_key":true,"lsp":{"auto_install":false}}' > "$XDG_CONFIG_HOME/cortexkit/aft.jsonc"
  unset ORT_DYLIB_PATH SUBC_CONNECTION_FILE
  export AFT_BINARY_PATH="$PWD/.tmp-375/release/aft-darwin-arm64"
  export AFT_ISSUE_375_ISOLATED=1 AFT_LOG_STDERR=1
  export CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER=
  bun docs/investigations/issue-375/plugin-probe.ts .tmp-375/release/package/dist/index.js
)
```

For Linux, run the same subshell in `docker run --rm --platform linux/amd64 --user root -v "$PWD:$PWD" -w "$PWD" oven/bun:1.4.2 sh`, install `git ca-certificates` inside the container first, choose a separate HOME, select `aft-linux-x64`, and set `PROBE_WARM_MS=20000`. Mounting at the same absolute path keeps the dependency symlink valid. No container writes to the parent checkout. The Linux release run built both indexes from empty storage.

For main, build with `CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo build -p agent-file-tools`, select `$PWD/target/debug/aft`, and pass `packages/opencode-plugin/src/index.ts`. Set `PROBE_HOST_DELAY_MS=3000` to reproduce the host-history latency. Raw local run artifacts were collected in `.tmp-375/` and retained after verification under ignored `target/issue-375/` (not committed).

## Exact config path

Paths/line numbers below refer to main unless explicitly labelled tag.

1. `packages/opencode-plugin/src/config.ts:1484-1545`: JSONC read/migration, retired-key translation, `AftConfigSchema.safeParse`; failure logs the warning and calls `parseConfigPartially`, **not** `return null`.
2. `config.ts:1328-1371`: parse each top-level key independently; retain successful keys. An ordinary unknown key fails alone. Valid `indexes` and `lsp` survive. A bad nested field can discard its whole top-level section; an unknown *top-level* key does not.
3. `config.ts:2030-2069`: merge user/project tiers and active harness overrides, resolve indexes. `packages/aft-bridge/src/feature-config.ts:665-668,701-721`: all three default true; trusted user-harness switches may replace values, project switches may only turn them off.
4. `config.ts:1983-2007`: `buildConfigTierConfigureParams` reads raw tiers, not the resolved Zod object. `bridge-bootstrap.ts` feeds these to the transport pool. Rust independently resolves them.
5. `crates/aft/src/config_resolve.rs:859-882,948-959`: strict `RawAftConfig` deserialization fails on the unknown key, then per-key `parse_config_partially` merges successful sections. Tag has the same mechanism (partial parser at tag line 885). `apply_harness_override`, `merge_trusted_config`, `merge_project_config`, then `apply_resolved_config` resolve flags; main lines 1679-1684 use defaults for absent leaves.
6. The resulting `config.indexes` controls configure's search/semantic startup. The two build events above are direct runtime evidence that flags survived in Rust, not just schema assertions.
7. Main differs for **live reload**: `config-live-reload.ts:46-58` refuses a candidate with validation errors and keeps last-good state; Rust `strict_tier_error` at `config_resolve.rs:896` does likewise for reload callers. Startup remains partial/lenient. Index switches are not a reason to infer a per-read reload. Release/main source diff does not change the startup partial parser.

A retired key with a translation error is a different category (`ConfigRejectedError`/Rust resolver errors), not the ordinary unknown-key warning under test. The reporter's actual key and project/harness overrides remain unknown.

## What the degraded strings mean (and do not mean)

The release already has `SearchLaneStatus::refusal` (`semantic_search/mod.rs`, tag lines 860-890). Normal search with neither retrieval lane ready returns `search_lanes_unavailable`; both explicitly off returns `no_search_lanes_enabled`. It does **not** normally proceed to the historical short walk. Main keeps this contract at lines 861-890 and invokes it at 1075-1077.

`fully_degraded` is still possible on explicit fallback paths (for example artifact-lock contention before that refusal). Tag `artifact_contention_fallback_response:3907` delegates to `semantic_unavailable_grep_fallback_response:4043`; that calls `execute_degraded_grep_fallback:4210`, a literal **content** scan, sets `fully_degraded:true`, and forces `complete:false`. It cannot supply semantic intent ranking and is not a filename enumerator. The formatter (`subc_format.rs:1790-1837`) renders those booleans as `fully degraded; partial/incomplete`; it does not diagnose *why* the config or indexes are unavailable.

However, tag fallback limits are **1,000 files / 100 results / 10 seconds** (`semantic_search/mod.rs:138-140`), and generic grep fallback is **50,000 files / 10 seconds** (`grep_executor.rs:24-27`). The exact `walk truncated at 200 files` wording is the legacy **outline directory formatter** (`subc_format.rs`, `format_outline_files_text`), not evidence of an unknown-config-key branch in aft_search. There are also search result/candidate caps that legitimately produce `200` without meaning 200 files. The issue supplies an agent's table, not raw search JSON; version skew, an older live process, another root, or attribution of an outline/candidate limit should be investigated, not asserted as fact.

Disk totals of 275 MB/858 MB are global historical storage, not proof that the active root's indexes were enabled, current, loaded or owned by that process. No search/semantic events in the excerpt is compatible with several states (already built, disabled by another tier, borrowed/missing snapshots, failed startup); it cannot select the config-error hypothesis.

## Read pre-phase: separate mechanism and evidence

`tool-perf.ts:64-82` starts the timer at execute entry; `_shared.ts:302-313` marks bridge start only after session-directory resolution. Thus `pre` includes:

- cold session-directory SDK lookup (`resolveProjectRoot`, cached afterward);
- synchronous path/realpath containment and optional external-directory/artifact permission checks;
- awaiting `context.ask` for read permission;
- **every read**, including README/source text: `hoisted.ts:489-493` awaits `currentSessionVisionCapability`;
- that function (`hoisted.ts:100-130`) awaits `resolvePromptContext`, then `client.provider.list()` if a model was found;
- `shared/last-assistant-model.ts:123-159` calls `client.session.messages({path:{id},query:{limit:50}})` each time. It is bounded to 50, not the entire session, but there is no cached/single-flight host lookup here and no timeout on either awaited SDK call.

Permissions can legitimately take seconds, and path/filesystem or event-loop stalls are also possible. But the read-specific extra SDK round trips are absent from ordinary grep/search/edit and match the report's asymmetry. Parallel reads each repeat them. Config validation is performed at bootstrap/project acceptance or message/reload hooks, not by `createReadTool` or the vision helper. Warnings recur around chat-message timestamps in the issue; they are not evidence of a multi-second config parse on each read.

Controlled released-plugin experiment, same invalid config and immediate permissions:

```text
host history delay 0ms: perf tool=read total=106ms pre=1ms bridge=104ms post=0ms
host history delay 3000ms: perf tool=read total=3112ms pre=3003ms bridge=109ms post=0ms
hostCalls: [{"path":{"id":"probe"},"query":{"limit":50}}]
```

The unit characterization records `permission.read → session.messages → provider.list → bridge.read` for `README.md`. The proposed-contract red is opt-in:

```sh
AFT_ISSUE_375_RED=1 bun test packages/opencode-plugin/src/__tests__/issue-375-read-pre.test.ts
```

Observed: **1 pass, 1 fail**. Exact failing test: `text read does not fetch host history or model catalog`; expected `[permission.read, bridge.read]`, received the extra history/catalog calls. No production mutation is needed: the existing implementation itself violates that proposed latency contract. Normal invocation passes the characterization and skips the opt-in red. This is a red for the verified read defect, not a fabricated red claiming the disproved unknown-key/index chain.

## Proposed fixes / next evidence

1. **Operator decision, not implemented:** choose whether ordinary unknown keys remain ignored-with-warning, trigger a visible degraded/config-error state, or refuse startup. Current partial behavior preserves indexes; changing it will not by itself explain this incident. If preserving partial config, make warnings name the ignored key and effective retained settings in an agent/user-visible status, consistently in Zod and Rust. Preserve the separate security/retired-key rules. Main's reload-vs-startup difference should be explicit.
2. **Read:** avoid history/catalog retrieval for ordinary text/directory reads. Prefer current model state from host hooks and a provider/model-keyed capability cache; coalesce concurrent lookups. Retain fresh capability on model switches and safe text-only behavior when unknown. If media cannot be identified safely before dispatch, negotiate media capability in a separate bounded phase rather than making every text read depend on host history. Add stage timings for permissions, directory lookup, model/history and catalog so the user's precise stall is attributable. Do not skip permissions.
3. **Search follow-up:** ask for one raw aft_search response (including lanes, readiness/reasons and root), the bridge spawn path and version/hash from the same session, project `.cortexkit/aft.jsonc` and active `harnesses.opencode` overrides, and startup search/semantic events. Restarting the host to exclude an older in-memory binary is a diagnostic control, not a promised fix. Compare root cache identity/ownership to existing artifacts before deleting any indexes.
4. The existing `linux-search-readiness.md` investigation records a distinct cold planner race already addressed on main. Its signature is an empty response with conflicting plan-vs-lane readiness, **not** the reported fully-degraded 200-file walk. Do not merge those diagnoses without raw evidence.

## Verification

- Released Linux x64 and macOS arm64 plugin-path probes and config controls above executed successfully; reported incident signature not reproduced.
- `CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo build -p agent-file-tools` passed (main).
- `bun install --frozen-lockfile` passed; no manifest/lockfile changes.
- `bun run --cwd packages/aft-bridge build` passed.
- `bun run --cwd packages/opencode-plugin typecheck` passed (`tsc --noEmit` and scripts project).
- Standalone `tsc --noEmit --module esnext --moduleResolution bundler --target esnext --types bun-types --skipLibCheck docs/investigations/issue-375/plugin-probe.ts` passed.
- Scoped AFT diagnostics: zero errors/warnings; Tier-2 unavailable, not treated as a full clean health result.
- Characterization passed; opt-in red failed exactly as described. No existing expectation was rewritten.
