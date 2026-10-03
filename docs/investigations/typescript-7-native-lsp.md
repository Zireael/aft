# TypeScript 7 projects: "Could not find a valid TypeScript installation" (2026-09)

Status: investigation and options only. No product change. Code references are to base `d34d59486`.
Every probe ran on a copy of the project under `/tmp/ts7probe.s2c9`. The real `common-auth` tree was only read.

## Summary

- **Cause.** TypeScript 7.0.2 is the native Go compiler, and its npm package ships no `lib/tsserver.js`,
  `lib/typescript.js` or `lib/tsserverlibrary.js`. AFT only knows how to start `typescript-language-server`,
  which needs a `tsserver.js`, so it fails in `initialize`. The wording of the error depends on which
  `typescript-language-server` AFT resolves:
  - 5.x (on `PATH` here) reports "Could not find a valid TypeScript installation". This matches the report.
  - 6.0.0 (AFT's auto-install cache here) reports "The TypeScript of the workspace (TypeScript 7.0.2 at …)
    provides no tsserver.js. No other valid TypeScript installation was found."
- **The cache fallback is broken too.** AFT's auto-installed fallback SDK (`typescript-sdk`) is unpinned. It
  now resolves to `typescript@7.0.2`, which has no `tsserver.js` either. So the fallback that should cover
  dependency-free worktrees no longer works for anyone, TS 5 projects included.
- **TypeScript 7 includes its own language server.** Start it with `tsc --lsp --stdio`. It is not listed in
  `--help`. It identifies itself as `typescript-go 7.0.2` and supports hover, definition, references, rename
  and more. It returns diagnostics for source files only when pulled (`textDocument/diagnostic`), with no
  workspace-wide pull. The only push it sent in this probe was for `tsconfig.json`.
- **AFT can already drive it through a custom server.** With `lsp.servers` pointing at `tsc --lsp --stdio`
  and the built-in `typescript` server disabled, `lsp_diagnostics`, post-edit diagnostics and scoped
  `inspect` all returned the same errors as `tsc --noEmit`. But `lsp.servers` is user-only and applies to
  every project, and TS 5's `tsc` rejects `--lsp` (TS5023). So this works only as a manual workaround.
- **AFT has one TypeScript server today.** `ServerKind::TypeScript` always starts
  `typescript-language-server --stdio`. Biome and Oxlint run alongside it and don't replace it.
- **Recommendation: option (a).** When the nearest `node_modules/typescript/package.json` reports major
  version 7 or later, start the native `tsc --lsp --stdio`. When it can't be started, report the named gap
  from option (b). Separately, pin the `typescript-sdk` auto-install below 7 so the cache fallback
  works again for TS 5 and 6 projects.

## 1. Reproduction

### Setup

```sh
T=/tmp/ts7probe.s2c9                       # mktemp -d
cd ~/Work/Projects/CortexKit/common-auth   # HEAD 7786746; read only
rsync -a --exclude .git --exclude node_modules ./ $T/common-auth/
cd $T/common-auth && bun install           # bun 1.4.2: "+ typescript@7.0.2", 113 packages
git init -q && git add -A && git commit -qm probe   # the copy only
cp ~/Work/Projects/CortexKit/aft/target/release/aft $T/aft   # `aft --version` → aft 0.58.1
```

A small Python driver (`$T/drive.py`) starts `aft` with no subcommand, which is the NDJSON-over-stdin mode
(`crates/aft/src/main.rs:450-515`). It sets isolated `AFT_STORAGE_DIR`, `AFT_CACHE_DIR`,
`FASTEMBED_CACHE_DIR` and `XDG_CACHE_HOME` under `$T/state*`, then sends:

1. `configure` with `harness: "opencode"` and a user tier setting `indexes.semantic: false`. When given,
   `lsp_paths_extra` is added, as the plugin would.
2. `lsp_diagnostics` for `test/fixtures/scratch.ts`, which spawns the TypeScript server.
3. `inspect` scoped to `package.json`, `tsconfig.json`, `tsconfig.build.json` and
   `test/fixtures/scratch.ts`, with section `diagnostics`. This is the reported call.
4. `lsp_inspect` for the same file.

Only a spawn attempt makes inspect report a TypeScript failure. Inspect itself never opens files or spawns
servers (`crates/aft/src/inspect/diagnostics_category.rs:171-177`). The reporter's session had already
opened a file, and step 2 does the same here.

### Run B: `typescript-language-server` 5.3.0 from `PATH` (matches the report)

`python3 drive.py $T/aft $T/common-auth "" $T/stateB`. `lsp_inspect` shows
`binary_path: ~/.local/share/mise/shims/typescript-language-server`, `binary_source: "path"`.
Inspect gap:

```
"kind": "failed_producer", "producer": "typescript",
"reason": "server not ready: TypeScript SDK unavailable: the language server could not find a valid
TypeScript installation; run bun install or enable LSP auto-install (AFT never installs into the worktree).
server failed during initialize: server error -32603: Request initialize failed with message: Could not
find a valid TypeScript installation. Please ensure that the \"typescript\" dependency is installed in the
workspace or that a valid `tsserver.path` is specified. Exiting."
```

Scoped files served by TypeScript, such as `test/fixtures/scratch.ts`, appear as `uncovered_file` gaps with
the same reason as their cause.

### Run A: `typescript-language-server` 6.0.0 from the auto-install cache (what the plugin passes)

`lsp_paths_extra` was set to `~/.cache/aft/lsp-packages/typescript-language-server/node_modules/.bin` and
`~/.cache/aft/lsp-packages/typescript/node_modules/.bin`. `lsp_inspect` shows
`binary_source: "lsp_paths_extra"`. Inspect gap:

```
"kind": "failed_producer", "producer": "typescript",
"reason": "server not ready: server failed during initialize: server error -32603: Request initialize
failed with message: The TypeScript of the workspace (TypeScript 7.0.2 at
\"/private/tmp/ts7probe.s2c9/common-auth/node_modules/typescript/lib\") provides no tsserver.js. No other
valid TypeScript installation was found. Exiting."
```

This cache install is inconsistent: its `package.json` asks for `typescript-language-server: 5.2.0`, but
6.0.0 is installed. This doesn't change the conclusion, since every version needs a `tsserver.js`.

### Where each part of the message comes from

| Part | Source |
|---|---|
| "Could not find a valid TypeScript installation. Please ensure …" and "… provides no tsserver.js. No other valid TypeScript installation was found." | `typescript-language-server`, in `LspServer.initialize` after `findTypescriptVersion(tsserver?.path, …)`. It checks the workspace folders under `MODULE_FOLDERS = ['node_modules/typescript/lib', …]` for `tsserver.js`, then its own bundled `typescript`, and throws if neither exists (`typescript-language-server/lib/cli.mjs` in the cache). |
| "TypeScript SDK unavailable: … run bun install or enable LSP auto-install …" | AFT, in `typescript_initialize_failure_reason` (`crates/aft/src/lsp/manager.rs:3390-3396`). It matches only the 5.x wording, so the 6.x wording reaches the agent unexplained. `is_environmental_message` also matches only the 5.x wording (`crates/aft/src/lsp/environmental.rs:20-23`). |
| "server failed during initialize: server error -32603" and the "server not ready:" prefix | AFT's initialize failure path (`crates/aft/src/lsp/manager.rs:3818-3840`), which calls `typescript_initialize_failure_reason` for `ServerKind::TypeScript`. `absorb_spawn_failure` (`manager.rs:3237-3242`) wraps the result as `LspError::ServerNotReady`. |

### Why AFT's SDK resolution doesn't help

`typescript_runtime_options` (`crates/aft/src/lsp/manager.rs:3323-3388`) runs before every
`typescript-language-server` spawn (`manager.rs:3170-3190`). It works in three steps:

1. **Project SDK.** `find_project_typescript_sdk` (`manager.rs:3446-3465`) walks up from the source file,
   stopping at the project root. It accepts a `node_modules/typescript/lib` only if that directory contains
   `tsserverlibrary.js` or `typescript.js`. TS 7's `lib/` contains neither, so the walk returns `None`.
   The caller then also requires `tsserver.js` (`manager.rs:3345-3346`).
2. **Cache SDK.** For each `lsp_paths_extra` entry, it checks `<entry>/../typescript/lib/tsserver.js`
   (`manager.rs:3349-3355`). The plugin fills that cache from the `typescript-sdk` entry
   (`packages/opencode-plugin/src/lsp-npm-table.ts:50-58`, and the same entry in `packages/pi-plugin`). That
   entry has no version cap, so auto-install picks the newest version older than the grace period
   (`packages/opencode-plugin/src/lsp-auto-install.ts:211-237`). That is now `typescript@7.0.2`: this
   machine's `~/.cache/aft/lsp-packages/typescript/node_modules/typescript/lib` holds only `getExePath.*`,
   `tsc.js` and `version.*`, and `.bin/` holds only `tsc`.
3. **Neither found.** AFT passes no `tsserver.path` and records the runtime note
   "TypeScript: server-managed SDK resolution (version not reported by AFT)" (`manager.rs:3357-3361`).
   `typescript-language-server` then fails as shown above.

A project that has no `node_modules` at all, even one pinned to TS 5, also fails at step 2 now.

## 2. What TypeScript 7.0.2 ships

Installed by `bun install` from `common-auth`'s lockfile on darwin-arm64:

```
node_modules/typescript/                      version 7.0.2, "type": "module"
  bin/tsc                                     #!/usr/bin/env node; imports ../lib/tsc.js
  lib/tsc.js                                  finds the native binary via getExePath(), then execve()s it
  lib/getExePath.js / .d.ts                   resolves @typescript/typescript-<platform>-<arch>/lib/tsc
  lib/version.cjs / .d.cts                    package "." export
  dist/                                       unstable JS API (api/, ast/, enums/) over the native process
  vendor/vscode-jsonrpc/
  package.json                                "bin": {"tsc": "./bin/tsc"}; optionalDependencies on 20
                                              @typescript/typescript-<os>-<cpu>@7.0.2 packages
node_modules/@typescript/typescript-darwin-arm64/   version 7.0.2, "os": ["darwin"], "cpu": ["arm64"]
  lib/tsc                                     Mach-O 64-bit executable arm64 (the compiler and server)
  lib/lib.*.d.ts                              the standard library declarations
```

There is no `tsserver`, `tsserver.js`, `tsserverlibrary.js` or `typescript.js` anywhere in either package,
and `node_modules/.bin` has `tsc` only. `getExePath.js` names the binary `tsc` for the `typescript` package
and `tsgo` for any other package name (such as the preview builds). It resolves the platform package
through `import.meta.resolve("@typescript/typescript-<platform>-<arch>/package.json")` and uses `lib/tsc`
next to it, adding `.exe` on Windows.

### The native language server

`tsc --help` and `tsc --help --all` don't list a server mode. The only hit for "server" in `--help --all`
is a note about the 20 MB JavaScript cap "in the TypeScript language server". The binary does have two
subcommands:

```
$ node_modules/@typescript/typescript-darwin-arm64/lib/tsc --lsp --help
Usage of lsp:
  -pipe string      use named pipe for communication
  -pprofDir string  Generate pprof CPU/memory profiles to the given directory.
  -socket string    use socket for communication
  -stdio            use stdio for communication
$ … lib/tsc --api --help
Usage of api:        (-async, -callbacks, -cwd, -pipe, -timing: the programmatic API server)
```

`node_modules/.bin/tsc --lsp --help` prints the same text through the wrapper. TS 5.9.3's `tsc --lsp --stdio`
fails with `error TS5023: Unknown compiler option '--lsp'` and exits 1.

A plain LSP client (`$T/lspprobe.mjs`) ran `node_modules/.bin/tsc --lsp --stdio` in the copy. It sent
`initialize`, `initialized`, and `didOpen` for `test/fixtures/err.ts`, then waited 4 s and sent pull,
hover and definition requests. The fixture has a TS2322 on line 2 and a TS2304 on line 4:

- **serverInfo:** `{"name":"typescript-go","version":"7.0.2"}`. `positionEncoding: "utf-16"`, incremental sync
  (`change: 2`), `save: true`.
- **Advertised providers:** completion (with resolve), hover, signatureHelp, definition, typeDefinition,
  implementation, references, documentHighlight, documentSymbol, codeAction (`quickfix`,
  `source.organizeImports`, `source.removeUnusedImports`, `source.sortImports`, `source.fixAll`), codeLens,
  workspaceSymbol, document, range and on-type formatting, rename with prepare, foldingRange,
  selectionRange, callHierarchy, linkedEditingRange, semanticTokens, inlayHint, and
  `workspace.fileOperations.willRename`.
- **Diagnostics are pull-only for source files.** It advertises
  `diagnosticProvider: {identifier: "typescript", interFileDependencies: true, workspaceDiagnostics: false}`.
  `textDocument/diagnostic` returned `kind: "full"` with `2:TS2322 sev=1`, `4:TS2304 sev=1` and
  `4:TS6133 sev=4` (an unused-local hint). The only `publishDiagnostics` was one empty push for
  `tsconfig.json`, and that stayed true when the client did not declare pull support. `tsc --noEmit`
  reports the same two errors, and no hint.
- **Hover:** returned ```` ```typescript\nconst n: number\n``` ````. **Definition:** `join` resolved to
  `node_modules/@types/node/path.d.ts:64`.
- **Server-to-client traffic:** `workspace/configuration`, one `client/registerCapability` (for
  `workspace/didChangeConfiguration`, sections `js/ts`, `typescript`, `javascript`, `editor`), and many
  `window/logMessage`.

### AFT driving the native server today (custom-server workaround)

The driver ran with this user config and no `lsp_paths_extra`:

```jsonc
{ "lsp": { "disabled": ["typescript"],
           "servers": { "tsgo": { "extensions": ["ts","tsx","js","jsx","mjs","cjs","mts","cts"],
                                  "binary": "tsc", "args": ["--lsp","--stdio"],
                                  "root_markers": ["tsconfig.json","jsconfig.json","package.json"] } } } }
```

- `lsp_diagnostics test/fixtures/err.ts` returned `server_id: "tsgo"`, `status: "pull_ok"`, and 3
  diagnostics: TS2322 and TS2304 as `error`, TS6133 as `hint`.
- `edit_match … diagnostics: true` returned the same 3 with `lsp_complete: true`.
- The scoped `inspect` showed `by_producer.tsgo.errors: 2`, and `tsconfig.json` counted as covered through
  the pushed report. The remaining gaps were `package.json` and `tsconfig.build.json`, both for Biome, which
  has no `biome.json` in this project. None were for TypeScript.

AFT's existing support for pull diagnostics (`crates/aft/src/commands/lsp_diagnostics.rs:31-50`) and its
pull-to-store path already cover the native server. Inspect's coverage matches today's TypeScript: files
the server has analysed are covered. Going by the code, files never opened should get the
`RUNNING_WITHOUT_REPORT` gap (`diagnostics_category.rs:734-740`); this probe didn't run that case. This only works as a workaround, for two reasons. `lsp.servers` and
`lsp.disabled` are user-only (`docs/config.md:682-683`), so the setting applies to every project. And the
same config breaks every TS 5 or 6 project, where `tsc --lsp` fails with TS5023.

## 3. AFT's TypeScript server selection today

- There is one built-in TypeScript definition: `ServerKind::TypeScript`, which runs
  `typescript-language-server --stdio` for `ts`, `tsx`, `js`, `jsx`, `mjs` and `cjs`, with root markers
  `tsconfig.json`, `jsconfig.json` and `package.json` (`crates/aft/src/lsp/registry.rs:310-320`). There is
  no built-in kind for `tsgo`, `tsc --lsp` or `vtsls`.
- Biome and Oxlint also match JS and TS files. They are co-servers (linters and formatters), not
  TypeScript type checkers (`registry.rs:132-173`).
- The binary comes from project `node_modules/.bin`, then `lsp_paths_extra`, then `PATH`
  (`manager.rs:3257-3304`). The only per-project TypeScript choice AFT makes today is the SDK passed as
  `tsserver.path`. It never chooses a different server program.
- Custom servers (`lsp.servers`, `ServerKind::Custom`) can run any binary, which is what the workaround
  above relies on.

## 4. Options

### (a) Start the native server when the project's TypeScript is 7 or later

For each TypeScript server root, decide which program to run before building `PreparedSpawn`. If the
project's TypeScript is major version 7 or later, start
`<platform package>/lib/tsc --lsp --stdio` instead of `typescript-language-server`. Don't send the
`tsserver.path` initialization options, and record a runtime note such as
"TypeScript 7.0.2: native language server (project installation)".

Risks:
- **Mixed monorepos.** A TS 5 package and a TS 7 package in one project need different servers. The choice
  must be made per resolved `node_modules/typescript`, and that directory belongs in the server key, or
  one server will serve both.
- **Launching.** `node_modules/.bin/tsc` is a `#!/usr/bin/env node` wrapper, and Bun-only machines may not
  have `node` on `PATH`. Starting the platform binary directly avoids this and avoids a wrapper process
  (the wrapper uses `process.execve` on newer Node and falls back to a child process). That means copying
  the lookup in `getExePath.js`: `@typescript/typescript-<os>-<cpu>/lib/tsc[.exe]`, found from the
  typescript package's real path, which matters under Bun's isolated linker and pnpm. If the platform
  package is missing (an unsupported platform or a partial install), fall back to the named gap in (b).
- **Features.** File diagnostics are pull-only, and there is no `workspace/diagnostic`, so a directory-mode
  `lsp_diagnostics` covers only files already pulled. That is no worse than today's push-only TypeScript
  server, but it should be tested. `tsconfig.json` diagnostics arrive by push.
- **Unmeasured.** Its memory, CPU and thread use next to other servers, and how stable
  `workspace/configuration` handling and restarts are over a long session.
- **Scope.** Vue, Astro and Svelte servers that load TypeScript's JS API are not covered. `astro-ls` already
  fails with `ASTRO_TSDK_UNAVAILABLE` on TS 7, because `find_project_typescript_sdk` needs `typescript.js`
  (`manager.rs:3433-3434`). Those need their own decision.

### (b) Report TypeScript 7 as a named gap

Detect TS 7 as described below. If no native server is started (for example, AFT has no support for it
yet, or the platform package is missing), don't spawn `typescript-language-server`, which is known to fail.
Report a named gap instead, for example: `TypeScript 7.0.2 (native compiler) at node_modules/typescript
ships no tsserver; typescript-language-server cannot serve it. Run the project's tsc --noEmit for type
errors.` Also recognise the 6.x wording ("provides no tsserver.js") in `typescript_initialize_failure_reason`
and `is_environmental_message`, and drop the "run bun install or enable LSP auto-install" advice in this
case, because both have already happened.

Risks: low, since it only changes a message and skips one spawn that was going to fail. But TS 7 projects
still get no LSP diagnostics, hover or definition, and agents keep falling back to `tsc`, as the reporter
did. It is honest, but it is not a fix.

### (c) Fall back to a bundled or pinned TS 5.x `tsserver`

Pass `tsserver.path` pointing at a TS 5 or 6 SDK that AFT installs, whenever the project's TypeScript has
no `tsserver.js`.

Risks: high, because the results are wrong in ways nothing flags. With the same files and one tsconfig:

```
tsconfig {"compilerOptions":{"noEmit":true,"moduleResolution":"node","baseUrl":"."},"include":["src"]}
  TS 5.9.3: exit 0
  TS 7.0.2: TS5108 Option 'moduleResolution=node10' has been removed. …
            TS5102 Option 'baseUrl' has been removed. …                       exit 1
tsconfig {"compilerOptions":{"noEmit":true},"include":["src"]}
  src/a.ts: export function f(x) { return x; }  let s: string = undefined;
  TS 5.9.3: exit 0
  TS 7.0.2: TS7006 Parameter 'x' implicitly has an 'any' type.
            TS2322 Type 'undefined' is not assignable to type 'string'.        exit 1   (strict by default)
```

A TS 5 fallback reports clean on code that the project's own `tsc` rejects, from both removed options and
changed defaults. Hover and definition would also reflect the wrong compiler version. Any config or syntax
that only TS 7 accepts would show up as false errors. Even with a "not the project's pinned TypeScript" note
(`manager.rs:3379-3383`), agents act on the diagnostics, not the note. As a side effect, it would also need
the same `typescript-sdk` pin as the separate fix below.

## 5. Recommendation

Implement **(a)**, and use (b) as what it does when it can't start the native server. (b) on its own leaves
TS 7 projects without diagnostics. (c) is actively misleading, as shown above. (a) is low risk because the
custom-server run shows AFT's LSP client, pull path, post-edit wait and inspect store already work with
`typescript-go` 7.0.2 without changes. The remaining work is choosing the server per root, launching it
without the wrapper, and keying servers by root.

A separate change is needed whichever option is chosen: pin the `typescript-sdk` auto-install to
`typescript@<7` in both plugins' `lsp-npm-table.ts`. That cache exists to provide a `tsserver.js`, and
since `typescript@latest` became 7 it has none. Every dependency-free worktree has lost its TypeScript
diagnostics as a result, including TS 5 projects.

### Detection

1. Start from the triggering source file and walk the same ancestor chain as `find_project_typescript_sdk`,
   bounded by the project root, or by the server root when the file is outside the project. At each level,
   look for `node_modules/typescript/package.json` rather than `lib/*.js`, because TS 7's `lib/` has
   neither probe file. The nearest match is the project's TypeScript.
2. Parse its `version` field and take the leading integer as the major version (`"7.0.2"` → 7,
   `"7.1.0-dev.20260929.1"` → 7).
   - Major 7 or later: native path (a). Then resolve `@typescript/typescript-<os>-<cpu>/package.json` from
     the typescript package's real path, and use `lib/tsc` (`tsc.exe` on Windows). If that fails, use the
     gap in (b).
   - Major 6 or earlier, with `lib/tsserver.js` present: today's `typescript-language-server` path.
   - Unparseable version, or no `tsserver.js` and no platform package: gap (b), naming the version string
     and path that were found.
   - No `node_modules/typescript` at all: today's cache-SDK fallback, pinned below 7.
3. After `initialize`, check that `serverInfo.name` is `typescript-go`. If it isn't, report a named gap
   rather than trust the server.
4. **Never decide from the lockfile alone.** The lockfile records what should be installed, but
   `node_modules` is what runs. After a branch switch without `bun install`, the lockfile can say 7 while
   `node_modules` still holds 5, or the other way round. Following the lockfile would start `tsc --lsp`
   against a TS 5 install (TS5023) or `typescript-language-server` against TS 7 (the failure above). If
   `node_modules/typescript` is missing, keep today's "run bun install" gap. Don't guess from the lockfile.

## Appendix: probe files

All under `/tmp/ts7probe.s2c9`, outside the repository: `common-auth/` (the copy, with the fixture
`test/fixtures/err.ts` added), `drive.py` (the AFT NDJSON driver; env `EXTRA_DOC`, `PROBE_FILE`, `EDIT`),
`lspprobe.mjs` (the plain LSP client), `ts5/` (`typescript@5.9.3`) and `diverge/` (the config divergence
project). Tool versions: bun 1.4.2, node v26.7.0, darwin-arm64.
