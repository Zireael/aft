# aft_inspect truth at scale: baseline report

This report measures how often AFT's inspect findings match the authoritative tool for each
language. It covers six pinned open-source repositories in four languages. The harness is
`benchmarks/inspect-truth/run.py`. The generated tables and every sampled row are in
`results/results.md` and `results/results.json`. This file adds the judgement that a script
cannot make: for each sampled disagreement, whether AFT or the oracle is right, and why.

AFT binary: a release build of this checkout (`agent-file-tools` 0.58.2, base commit
`13dd9cb71`), built with `CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo build --release -p agent-file-tools`.

## What already existed, and what was reused

- `benchmarks/inspect-field-audit/` is a 12-repo inspect field audit. It records tool
  presence and hand-verified samples, but runs no oracle and computes no precision or
  recall. Reused: its NDJSON client (`NdjsonClient`) and Tier 2 readiness check
  (`pending_tier2`), imported directly. Two corpus entries were also reused
  (`soft-serve`, `fastapi`).
- No earlier fallow or knip comparison script, oracle normalizer or oxc-resolver spike was
  found in `docs/`, `benchmarks/`, `scripts/`, `spikes/` or `.cortexkit/alfonso/`. The oxc
  resolver now ships as production code (`crates/aft/src/inspect/oxc_engine/`).
  `~/Work/OSS/fallow` is a clone of fallow's source, not a harness.

## Corpus

Clones live in `~/Work/OSS/inspect-corpus/<name>`, outside this repo. `run.py fetch`
checks out the pinned commit.

| name | language | role | commit | why |
|---|---|---|---|---|
| outline | TypeScript | app, about 2,200 .ts/.tsx files | `d410db1b24594d71b766212282804d79ca18e5c1` | Large single-package React + Koa app with path aliases and plugins. App-style dead code with no published API. |
| typeorm | TypeScript | library, about 3,600 .ts files | `17e858da8d2becab1ca47826f670a4660da70142` | Widely used ORM. pnpm monorepo whose public API ships from a build directory (`publishConfig.directory`) and a `"./*"` subpath export. |
| ripgrep | Rust | workspace | `3fce3b5bb0236da2df6d99672afb8a719642eca7` | Ten library crates plus the `rg` binary crate. Covers bin-crate code and `lib.rs` re-exports. |
| axum | Rust | workspace, macro and trait heavy | `f8b02f22cf10bee707bda19b58265b9e33677535` | Built from blanket trait impls, `macro_rules!`-generated impls and types (`define_rejection!`, `all_the_tuples!`), plus a proc-macro crate. |
| soft-serve | Go | module | `37685d36f5b7bf0e32217ddd7c8e045c57772619` | Actively maintained Git server: cobra CLI, interfaces, internal packages. |
| fastapi | Python | package | `26dbe0b00e1211b99446500fc6e402cfec6a3b99` | Widely used typed Python package. |

Comparisons are limited to each repo's `include_paths` (in `corpus.json`). Test files are
dropped from both sides (see Method).

## Oracles

| category | language | oracle | status |
|---|---|---|---|
| unused exports, dead code | TS | `fallow dead-code --format json --include-entry-exports` (2.88.3), minus files `fallow list --entry-points` names | ran |
| second opinion | TS | `knip --reporter json` (installed in `~/Work/OSS/inspect-corpus/.tools/node`) | ran; used to label each disagreement |
| dead files | TS | fallow `unused_files` | ran; AFT has no dead-file category |
| dead code | Rust | rustc `dead_code` lint from `cargo +stable check --message-format=json` | ran; 0 warnings in both repos |
| dead code | Rust | `cargo +nightly udeps` | **not run**: not installed, and no new toolchain was installed for this run |
| dead code | Go | `staticcheck -checks U1000` (installed in `.tools/go-bin`) | ran; 0 findings |
| dead code | Go | `golang.org/x/tools/cmd/deadcode` (added; installed in `.tools/go-bin`) | ran; see below for why it was added |
| dead code | Python | `vulture --min-confidence 60` (installed in `.tools/py`) | ran |
| diagnostics | all | `tsc --noEmit`, `cargo check`, `go vet`, `pyright` (with fastapi's own `.venv`) | ran |
| todos | all | a comment-marker regex | ran; a cross-check, not authoritative |
| duplicates | all | none | **not scored**: no authoritative oracle was named |

Why deadcode was added for Go: U1000 treats every exported identifier as used, but AFT's
dead-code scanner only reports exported symbols. U1000 therefore cannot confirm or refute
any AFT finding; its 0 on soft-serve says nothing about AFT. `deadcode` walks the whole
program from `main` and does report exported functions. It is the Go team's own tool, and
like the other oracles it went into the scratch prefix.

## Method

1. **Full AFT lists.** Inspect's per-category item lists are capped at 100 rows. `topK` cannot
   raise the cap and there is no offset. A scoped inspect, though, rolls up the whole project,
   filters to the scope, and only then caps, while `summary.<category>.count` keeps the full
   in-scope count. The harness compares that count to the rows returned and splits the scope
   until each piece fits (a path list in halves, a directory into its children). Every
   category's collected total equals AFT's project-wide count, with one exception:
   `typeorm/packages/typeorm/src/driver/mongodb/typings.ts` holds 134 unused exports in one
   file, and no request can return more than 100 of them. That leaves 34 rows unrecoverable,
   recorded in `aft_truncated_scopes`.
2. **Language servers off during collection.** Every inspect call computes every category,
   whatever `sections` asks for, and a scoped call first has the language servers analyze up
   to 1,000 scoped files. With servers on, collecting typeorm's lists ran past 30 minutes;
   the first attempt hit the shell timeout. Collection therefore uses a user-tier config with
   `lsp.disabled` (the project tier drops that key). The diagnostics pass runs in a separate
   session with servers on. A full re-run reproduced every count exactly.
3. **Normalization.** Both sides become `(category, path, symbol, line)`. Rows pair on
   `(path, symbol)`; when a file declares a name twice, the nearest line wins. Diagnostics
   and todos pair on `(path, line)`.
4. **Test files** (AFT's own `is_test_tree_file` segments plus `test/`, `testing/`, `testdata/`)
   are dropped from both sides. AFT withholds dead symbols in them by design. The per-repo
   counts dropped are in `test_file_findings_excluded`.
5. **TS dead code vs unused exports.** fallow's unit is the unused *export*. An export used
   only inside its own file is an unneeded `export` keyword, not dead code. For the
   `dead_code` comparison those rows (185 in outline, 69 in typeorm) are removed from the
   oracle. When fallow reports a whole file unused, AFT findings in that file count as
   "agreed (file-level)".
6. **Cause hints.** Every disagreement gets a grep-based hint (namespace member reference,
   dynamic import, interface method, library public API, and so on). The hints count causes
   over whole buckets. The verdicts below were checked by reading the code.

## Results

Precision = (agreed + file-level agreed) / AFT count. Recall = agreed / oracle count.

| repo | lang | category | AFT | oracle | agreed | AFT-only | oracle-only | precision | recall |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| outline | TS | unused_exports | 223 | 289 | 221 | 2 | 68 | 0.99 | 0.77 |
| outline | TS | dead_code | 386 | 104 | 81 | 305 | 23 | 0.21 | 0.78 |
| outline | TS | dead_files | n/a | 307 | n/a | n/a | n/a | n/a | n/a |
| outline | TS | todos | 19 | 20 | 19 | 0 | 1 | 1.00 | 0.95 |
| outline | TS | diagnostics (60 files) | 1 | 0 | 0 | 1 | 0 | 0.00 | n/a |
| typeorm | TS | unused_exports | 121 | 78 | 17 (+4 file) | 100 | 61 | 0.17 | 0.22 |
| typeorm | TS | dead_code | 75 | 9 | 0 (+4 file) | 71 | 9 | 0.05 | 0.00 |
| typeorm | TS | dead_files | n/a | 19 | n/a | n/a | n/a | n/a | n/a |
| typeorm | TS | todos | 10 | 10 | 10 | 0 | 0 | 1.00 | 1.00 |
| typeorm | TS | diagnostics (60 files) | 0 | 0 | 0 | 0 | 0 | n/a | n/a |
| ripgrep | Rust | dead_code | 52 | 0 | 0 | 52 | 0 | 0.00 | n/a |
| ripgrep | Rust | todos | 11 | 11 | 11 | 0 | 0 | 1.00 | 1.00 |
| ripgrep | Rust | diagnostics (60 files) | 1 | 0 | 0 | 1 | 0 | 0.00 | n/a |
| axum | Rust | dead_code | 55 | 0 | 0 | 55 | 0 | 0.00 | n/a |
| axum | Rust | todos | 5 | 5 | 5 | 0 | 0 | 1.00 | 1.00 |
| axum | Rust | diagnostics (60 files) | 0 | 0 | 0 | 0 | 0 | n/a | n/a |
| soft-serve | Go | dead_code vs U1000 | 101 | 0 | 0 | 101 | 0 | 0.00 | n/a |
| soft-serve | Go | dead_code (functions) vs deadcode | 98 | 20 | 5 | 93 | 15 | 0.05 | 0.25 |
| soft-serve | Go | todos | 33 | 33 | 33 | 0 | 0 | 1.00 | 1.00 |
| soft-serve | Go | diagnostics | 0 | 0 | 0 | 0 | 0 | n/a | n/a |
| fastapi | Python | dead_code | 0 | 123 | 0 | 0 | 123 | n/a | 0.00 |
| fastapi | Python | todos | 17 | 17 | 17 | 0 | 0 | 1.00 | 1.00 |
| fastapi | Python | diagnostics (52 files) | 57 | 57 | 57 | 0 | 0 | 1.00 | 1.00 |

Duplicates are not scored. AFT reported 11 duplicate groups on soft-serve; the other repos'
counts are in `results.json` (`aft_duplicate_groups`).

### Reading the numbers after judging the samples

Raw precision and recall treat the oracle as truth. It is not always right, so here is how
the scores change once the judged samples are taken into account.

- **TS unused exports are good on an app** (outline 0.99 / 0.77). Most of outline's 68
  oracle-only rows are not real AFT misses. 30 are `export type` declarations. The sampled
  ones are used only in their own file, which AFT keeps on purpose
  (`oxc_engine/graph.rs:1037`); fallow and knip report them. Several more are fallow false positives (see "Where the oracle was wrong").
- **TS dead code over-reports badly**: 305 of 386 outline findings are wrong, 233 of them
  from a single cause. AFT's own `unused_exports` gets the same symbols right, so the dead-code
  reachability walk is the defect, not export analysis.
- **TS on a library is poor** (typeorm 0.17 / 0.05 precision). Every AFT-only row in the
  sample is public API.
- **Rust precision is effectively 0.** rustc reports no dead code in either workspace, so any
  `pub(crate)` or bin-crate item AFT flags is in use, and any plain `pub` item in a library
  crate is public API. All 107 Rust findings are false positives, with causes below.
- **Go precision is 0.05.** 93 of 98 function findings are reachable from `main`.
  AFT also misses 15 functions deadcode finds unreachable.
- **Python**: AFT has no Python dead-code lane, so recall is 0 against vulture's 123 rows.
  All 10 sampled vulture rows are vulture false positives, though: pydantic model fields
  read through serialization, and a protocol parameter (`*exc_info` in `__aexit__`). The
  practical miss rate is far below 123.
- **TODOs agree everywhere** (the one outline difference is a marker the regex matches
  inside a string).
- **Diagnostics agree where both sides ran.** fastapi matched pyright 57/57 once pyright
  used the project's venv. The disagreements are producer differences, not errors (below).

## Top causes of AFT errors

Ranked by the number of findings each explains in this corpus, AFT-only and oracle-only
combined, after reading the code.

### 1. Library public API is not treated as live (about 250 findings: TS 171, Rust about 80)

- **typeorm**: all 100 AFT-only unused exports and 54 of 71 AFT-only dead-code rows are in
  `packages/typeorm/src/driver/mongodb/typings.ts`. `package.json` publishes every file
  through `"exports": {"./*": ...}` from `publishConfig.directory: build/package`. AFT
  ignores `*` subpath patterns and does not map `./index.js` (no `dist/` or `build/` prefix)
  back to `src/index.ts`. Example: `typings.ts:5969 MongoClientBulkWriteExecutionError`.
  Another 13 dead-code rows are public error and decorator classes reached only through
  `src/index.ts` → `export * from "./error"` → `export * from "./ConnectionNotFoundError"`.
  Examples: `error/ConnectionNotFoundError.ts:6`, `decorator/listeners/AfterInsert.ts:8`.
  knip agrees with AFT on none of these rows.
- **Rust libraries**: `pub fn` methods on exported types, and `pub` items reached through
  `pub mod` or a glob `pub use …::*`, are flagged. Examples:
  `ripgrep crates/printer/src/json.rs:564 has_written`,
  `crates/globset/src/glob.rs:641 empty_alternates`,
  `axum/src/extract/ws.rs:913 into_text`, `axum-extra/src/either.rs:129 Either4`. The roughly
  22 axum rejection types defined inside `define_rejection! { … }` and re-exported by
  `pub use axum_core::extract::rejection::*` are the same failure, reached through a macro
  invocation (`axum/src/extract/rejection.rs:34 MissingJsonContentType`).

### 2. TS dead code ignores namespace member references (233 findings)

`import * as T from "./schema"` followed by a type-position use such as
`APIContext<T.CommentsUnresolveReq>` does not keep `CommentsUnresolveReq` live in the
dead-code walk. Outline routes every API schema type through this pattern: 233 rows, for
example `server/routes/api/comments/schema.ts:130 CommentsInfoReq` and
`server/routes/api/users/schema.ts:109 UsersNotificationsUnsubscribeReq`. AFT's unused-exports scanner handles the same imports (it marks namespace-imported modules
uncertain), so the two categories contradict each other on these symbols. knip marks
them used.

### 3. Go: uses through function values, closures and interfaces are not credited (108 findings: 93 false positives, 15 misses)

- Calls inside closures assigned to fields of package-level variables are not attributed to
  any live symbol. `cmd/soft/hook/hook.go:30` defines `Command = &cobra.Command{
  PersistentPreRunE: func(...) { cmd.InitBackendContext(c, args) … } }`. Everything
  reachable only from such closures is reported dead, which in a cobra CLI is most of the
  program: `cmd/cmd.go:19 InitBackendContext`, then `pkg/backend/backend.go:25 New`,
  `pkg/store/context.go:8 FromContext`, and on down. The cause hints count 57 such rows as
  "called elsewhere". The calls exist, but in code AFT considers dead.
- Function values are not counted as uses (17 rows): `PersistentPreRunE: cmd.InitBackendContext`,
  `ssh.go:73 CommandMiddleware,`, `http.HandlerFunc(GoGetHandler)`.
- Methods that satisfy an interface and are called only through it (9 rows):
  `pkg/store/database/webhooks.go:55 DeleteWebhookByID` implements `store.WebhookStore`.
- The reverse error comes from the name-based method-dispatch heuristic: 15 functions
  deadcode proves unreachable are kept live because the same method name is called on some
  other type. Examples: `pkg/ui/pages/repo/stashitem.go:45 Less` (sort.Interface on a type
  never sorted) and `cmd/soft/serve/server.go:259 Close`.

### 4. TS dead code does not follow dynamic `import()` (70 findings)

`app/hooks/useSettingsConfig.ts:33 lazy(() => import("~/scenes/Settings/ApiKeys"))` loads a
default export the dead-code walk never reaches, so
`app/scenes/Settings/ApiKeys.tsx:159 default` is reported dead. All 70 rows are default
exports of modules that some file loads with `import("…")`, according to the grep hint. The
sampled ones are lazily loaded settings scenes and plugin pages. Unused exports does not
flag them.

### 5. Rust: calls through `use … as` aliases and inside `macro_rules!` bodies (about 21 findings)

- ripgrep's `crates/core/flags/mod.rs:18` re-exports `bash::generate as generate_complete_bash`
  and ten more aliases, and `main.rs` calls `flags::generate_complete_bash()`. None of the
  aliased functions is credited (`complete/bash.rs:59 generate`, `doc/help.rs:119 generate_long`,
  `doc/version.rs:10 generate_digits`), and neither is anything reachable only from them
  (`flags/parse.rs:147 lookup`): about 13 rows.
- `crates/core/messages.rs:100 messages`, `:112 ignore_messages` and `:133 set_errored` are
  called only from inside the crate's `message!`/`err_message!` `macro_rules!` bodies.
- A handful of `Type::assoc()` and `module::func()` calls in ripgrep are not credited
  (`printer/src/util.rs:191 Sunk::from_sink_match`, `searcher/src/lines.rs:115
  lines::without_terminator`). Their callers are live trait-impl or crate-private methods, so
  these look like path-call resolution misses that remain after AFT's earlier fix for
  path-expression calls. Confidence
  is lower here: the callers are not themselves visible in AFT's output.

Also seen, at lower counts:

- `export type` used in its own file is kept on purpose (about 30 outline rows).
- Unused default exports of live modules are not reported by dead code
  (`shared/editor/lib/markInputRule.ts:21`).
- A vendored `.ts` file of `export declare …` ambient declarations is treated as real
  exports: only `.d.ts` ambient exports are skipped (`graph.rs:1128`). This adds to cause 1
  on typeorm.

## Where the oracle was wrong

- **fallow, namespace member use**: `app/components/primitives/components/Menu.tsx:109
  MenuSubTrigger` is used as `Components.MenuSubTrigger` (`Menu/index.tsx:142`). fallow reports
  it unused; knip and AFT get it right.
- **fallow, member access on a dynamic import**: `server/converters/CsvConverter.ts:9` is
  loaded by `(await import("./CsvConverter")).CsvConverter`. Same for the `server/services/*`
  defaults, which `server/main.ts:117` loads through a lazy map.
- **fallow, unused files**: 306 of outline's 311 unused files are Sequelize migrations, which
  sequelize-cli loads by directory. typeorm's command classes are imported by `src/cli.ts`,
  but fallow does not map the `bin` entry `./cli.js` to source, so it reports
  `commands/CacheClearCommand.ts:12` and its siblings unused. AFT is right on all of these.
- **fallow's default mode cannot be used for apps**: its plugins turned 2,228 of outline's
  2,231 files into entry points, which reports 0 unused exports. This is why the harness
  uses `--include-entry-exports` and removes only the files fallow itself lists as entries.
- **vulture** on fastapi: all 10 sampled rows are false positives (pydantic fields, protocol
  parameters).
- **rustc** cannot see public API. Its 0 is right for these workspaces, but a library item
  rustc stays silent on is not thereby used; each AFT row was judged by reading the code.

## Diagnostics notes

- The outline AFT-only row is an oxlint lint warning (`useShareDataLoader.ts:30`, setState in
  an effect). tsc has no such check. This is a producer difference, not an error.
- The ripgrep AFT-only row is correct: `crates/globset/benches/bench.rs:5 #![feature(test)]`
  fails on stable. rust-analyzer checks bench targets; the oracle skips them because they
  break `cargo check --all-targets` on stable.
- **typeorm: AFT produces no TypeScript diagnostics at all.** The project-root server reports
  `TypeScript SDK unavailable … Could not find a valid TypeScript installation`, because pnpm
  installs `typescript` under `packages/typeorm/node_modules`, not at the root. The gap is
  named, but `E`/`W` read 0 for every TS file in the repo.
- **Go: AFT has no Go diagnostics producer here** (`gopls is unavailable`). Nothing is
  measured; `go vet` is clean.

## Tool problems to pass to the AFT owner

1. Every scoped inspect call recomputes the full uncapped rollup and runs the scoped
   diagnostics sweep, even when `sections` excludes diagnostics. That is 6 s per call with
   servers off and 40-60 s with them on (typeorm). Input: `{"command":"inspect","sections":["dead_code"],"scope":["packages"]}` on
   typeorm. Output: a `diagnostics` summary with `coverage.examined: 1000`. Expected: no
   diagnostics work when diagnostics is not requested. The full typeorm collection took 84
   inspect calls.
2. Lists are hard-capped at 100 with no offset, and the cap applies per scope. A single file
   with more than 100 findings can never be listed in full (typeorm `typings.ts`: 134, 100
   returned).
3. On a cold repo, inspect returns `dead_code: {"unavailable": true, … "inspect_phase_timeout
   … builder_state=building"}`. The `pending_categories` list stays empty, so a client that
   polls `scanner_state` believes the result is final.
4. Test-only rows carry a `used_by` list that can include non-test files.
   `cmd/soft/serve/server.go NewServer` has `used_by: [certreloader_test.go, server.go]` yet is
   test-only. The real non-test caller sits in a closure assigned to a package-level variable (cause 3 above), so the label is
   misleading.

## How to re-run

```
CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo build --release -p agent-file-tools
python3 benchmarks/inspect-truth/run.py all            # every repo, one at a time
python3 benchmarks/inspect-truth/run.py aft --repo axum # one phase, one repo
python3 benchmarks/inspect-truth/run.py score           # re-score from saved raw output
```

Oracle binaries are expected in `~/Work/OSS/inspect-corpus/.tools`:

- `go-bin/staticcheck` and `go-bin/deadcode`: `GOBIN=… go install …@latest`
- `py/bin/vulture`: `uv venv py && uv pip install vulture`
- `node/node_modules/.bin/knip`: `npm install --prefix node knip typescript`

Also needed: `/opt/homebrew/bin/fallow`, and `pyright`, `cargo` and `go` on `PATH`. The TS
repos and fastapi need their dependencies installed (`fetch` runs the TS installs; fastapi
uses `uv venv .venv && uv pip install -e .`). Raw tool output, AFT storage and full
disagreement lists go to `~/Work/OSS/inspect-corpus/_results/<repo>/`.
