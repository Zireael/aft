# Config live-reload inventory (2026-09)

Research only; no product code changed. This inventory lists every key the AFT config resolvers accept, who reads each one, when it is read, what a live change would have to do, and whether the model-visible prefix changes. The operator decides per key what reloads live. The grouping in [section 7](#7-proposed-grouping-proposal-for-the-operator) is a proposal only.

Worktree base: `bcda39d646dcf32ee413c60efebb8a5f1ca45d68`.

## Path abbreviations

| Prefix | Path |
|---|---|
| `R:` | `crates/aft/src/` |
| `OC:` | `packages/opencode-plugin/src/` |
| `PI:` | `packages/pi-plugin/src/` |
| `BR:` | `packages/aft-bridge/src/` |

Tier legend (from `R:config_resolve.rs`):

- **U**: user-only. A project value is dropped and reported (`record_project_drops`, `R:config_resolve.rs:1477-1587`).
- **T**: the project may only tighten (add a disable, switch an index off, enable the sandbox, add a read-deny, raise a timeout).
- **F**: free. The project value overrides the user value (`merge_project_config`, `R:config_resolve.rs:1059-1098`; TS allowlist `OC:config.ts:1742-1778`).

A `harnesses.<id>` block has the same tier as the file it sits in. The user harness block overrides the user base through `merge_trusted_config`. A project harness block goes through the same project filter. Disables only accumulate (`apply_harness_override`, `R:config_resolve.rs:899-957`; TS `OC:config.ts:670-682`, `PI:config.ts:42-74`).

---

## 1. Key findings

1. **Nothing watches either config file today.** Rust has no config watcher and no reload command. TypeScript has no `fs.watch`/chokidar/`watchFile` on `aft.jsonc`. The only `watch()` in plugin source is the TUI preferences file (`OC:tui/preferences.ts:200-216`). Neither file is polled by mtime.
2. **Config edits already take effect at uncontrolled moments, because every `configure` re-reads both files from disk.** `handle_configure` calls `resolve_config_tiers_for_configure`. That function reads `~/.config/cortexkit/aft.jsonc` and `<root>/.cortexkit/aft.jsonc`, and the file contents beat the tiers sent over the wire (`R:commands/configure.rs:2389-2409`, `R:subc_config.rs:59-108`). Configure runs on:
   - every new bridge spawn in standalone mode, where the per-bridge `projectConfigLoader` re-reads the tiers (`OC:bridge-bootstrap.ts:595-601`, `BR:pool.ts:480-489`);
   - every `BridgePool.reconfigure`, for example after LSP auto-install finishes (`BR:pool.ts:457-478`, `OC:lsp-auto-install.ts:750`, `PI:lsp-auto-install.ts:720`, `BR:revivable-transport.ts:125`);
   - every subc RouteBind in daemon mode (`R:subc/mod.rs:5627-5655`).

   In the daemon, one `AppContext` per root is shared by every session on that root. A new session binding the root therefore republishes the edited Rust-side config to all existing sessions on it, while every plugin's TS-side snapshot stays frozen. A mid-session edit is already applied halfway today. It just happens at the next bind rather than when the file changes.
3. **Rust stores config as an atomically swapped snapshot.** The field is `config: RwLock<Arc<Config>>` (`R:context.rs:2474`), read through `ctx.config()` (`R:context.rs:4636`) and published by `set_config` (`R:context.rs:4645`). Most Rust readers take a fresh snapshot on every call. Re-running the resolver and publishing is enough for them. A rejected candidate is never published: the last good config stays (`R:config_resolve.rs:810-818`, `R:commands/configure.rs:2721-2738`).
4. **The TS plugins freeze config at load.** OpenCode 1 passes `aftConfig` into `ctx.config` (`OC:index.ts:446-454`), and Pi does the same (`PI:index.ts:770-775`). Tools do read `ctx.config` on each call, but that object is the startup snapshot. TS-owned behaviour could be made live by swapping `ctx.config`. Tool registration, schemas, descriptions and the workflow-hints system text cannot, because they are built once.
5. **Keys that change the model-visible prefix:**
   - `disabled_tools`, `edit_mode`, `bash.compress`, `bash.background`, `bash.detach_on_user_message`, `bash.powershell_tool` (Pi), `pi.tool_presentation` (Pi/OMP), `backup.enabled`, `github.read` and `github.write`;
   - the legacy `experimental.bash.{rewrite,compress,background}` flags, through `resolveBashConfig`.

   Details are in section 4.
6. **Several accepted keys have no reader anywhere:**
   - `inspect.categories`, `inspect.tier2_soft_deadline_ms` and `inspect.max_drill_down_items` are parsed (`RawInspect`, `R:config_resolve.rs:430-457`; TS Zod `OC:config.ts:424-429`) but never reach `InspectConfig` (`R:config.rs:290-301`, `R:config_resolve.rs:1909-1934`). No TS code reads them.
   - Rust also accepts `configure_warnings_delivery`, `auto_update`, `bridge.*`, `pi.*`, `lsp.auto_install`, `lsp.grace_days` and `lsp.versions` without acting on them. These are plugin-owned.
7. **Keys the resolvers accept that are missing from `assets/aft.schema.json`:** `type_checker_timeout_secs` (`R:config_resolve.rs:111-112`), `index` (standing roots, `R:config_resolve.rs:122`) and `views` (`R:config_resolve.rs:123`). They are included in the table below.

---

## 2. How config flows today

### Rust engine

| Step | Site |
|---|---|
| User config path (daemon): resolved once at process start | `R:main.rs:236` → `R:subc_config.rs:48-52` |
| User config path (standalone): received per configure as `cortexkit_user_config_path` | `R:commands/configure.rs:2362-2380` |
| Read both files from disk (raw text; missing file = `{}`) | `R:subc_config.rs:64-108` |
| Daemon RouteBind reads the files and ignores wire tiers | `R:subc/mod.rs:5627-5655` |
| Configure: file tiers win over wire tiers | `R:commands/configure.rs:2389-2409` |
| Resolve (reset onto default, keep process-state fields) | `R:config_resolve.rs:775-847` |
| Rejected candidate keeps the last good config | `R:commands/configure.rs:2721-2738` |
| Publish | `ctx.set_config` `R:commands/configure.rs:3410` (full path), `:2976` (LSP-paths-only path) |

`handle_configure` has three exits after resolving (`R:commands/configure.rs:2627-3706`):

- **Identical config** (including runtime-only fields). Registers the hashline binding and the session only. Nothing is republished (`:2863-2955`).
- **Same fingerprint and same warm key.** Refreshes workspace manifests and binds the session (`:3092-3219`). Any change to a config key changes the fingerprint (`configure_fingerprint`, `:2411-2434`), so a real edit never takes this path.
- **Full path**, which needs exclusive use of the root (`:3223`). It runs:
  1. `agent_child_env::maintain`, which installs or removes the gh shim and the git hooks (`:3238`);
  2. ONNX runtime lookup (`:3245-3249`);
  3. `set_config` (`:3410`), the backup policy (`:3423-3436`) and the warm-key check (`:3454-3470`).

   If the warm key changed, it also drops and reloads artifacts (`:3560-3664`), resets the tier-2 scheduler (`:3487`) and queues maintenance with the flags at `:3667-3700`.

The **warm key** (`configure_warm_key`, `R:commands/configure.rs:2436-2458`) covers `indexes.trigram`, `search_index_max_file_size`, the whole `semantic` struct, `views.enabled`, `indexes.callgraph`, `callgraph_chunk_size` and `inspect.enabled`. Changing any of these is treated as a new generation:

- the in-memory search index is cleared and reloaded from its artifact;
- the semantic index and cached embedding model are dropped, unless the semantic build inputs are unchanged and an in-flight build is adopted (`:3461-3465`, `:3601-3643`);
- the callgraph store is dropped and force-rebuilt only if the callgraph build key changed (`:2464-2483`, `:3614-3636`).

Changing any other key takes the full path with an equivalent warm key: `set_config` plus policy pushes, with no artifact reload.

### TypeScript plugins

| Host | Load | Registration / prefix build |
|---|---|---|
| OpenCode 1 | `resolveBootstrapConfig` at plugin init (`OC:index.ts:272-281`), then `loadAftConfig` (`OC:config.ts:1991-2053`, `loadConfigFromPath` `:1451-1522`) | Tools are returned from the plugin function (`OC:index.ts:726-734`, `tool: allTools` at `:845`). Bash description is overridden at `:806-816`. Hints are built once at `:822` and appended on every `experimental.chat.system.transform` (`:846-855`). |
| OpenCode 2 | `bootLocation` per host Location (`OC:entry/server-runtime.mjs:46-117`) | `registerAftTools` → `context.tool.transform` (`OC:tool-registration.ts:179-195`). The entry's comment says the host re-runs the server effect "when it reloads" a Location (`server-runtime.mjs:127-133`), which reloads config and re-registers tools. What triggers a Location reload was not verified. |
| Pi | `resolvePiBootstrapConfig` in the extension factory (`PI:index.ts:397`, `PI:config-error-state.ts:90-109`) | `registerPiToolSurface` (`PI:index.ts:890`, `PI:tool-registration.ts:231-257`). Hints are built once and appended on each `before_agent_start` (`PI:workflow-hints.ts:194-226`). Pi accepts `registerTool` after load: the plugin registers PowerShell at `session_start` (`PI:index.ts:896-911`). |

Tier text sent to Rust: `buildConfigTierConfigureParams` sends raw file text plus the user config path (`OC:config.ts:1952-1977`, `PI:config.ts:1952-1977`). `resolveProjectOverridesForConfigure` (`OC:config.ts:797`) is exercised only by tests. No production caller was found.

---

## 3. Existing partial reload paths (confirming the evidence)

The evidence said no config watching exists. That is **confirmed**, with these partial re-read paths:

| Path | What re-reads | Effect |
|---|---|---|
| Any Rust `configure` | Both files from disk (`R:commands/configure.rs:2389-2409`) | Every Rust-side key is republished for that root |
| Daemon RouteBind | Both files (`R:subc/mod.rs:5631-5634`); `lsp.diagnostics_on_edit` is captured into `RootMeta` (`:5642-5647`, `:5203-5208`) | Shared root context reconfigured for all its sessions |
| Standalone bridge spawn / `pool.reconfigure` | `projectConfigLoader` (`OC:bridge-bootstrap.ts:595-601`, `BR:pool.ts:457-489`) | As above, standalone |
| OpenCode 1 `chat.message` | `loadAftConfigOrLastGood(sessionDir)` (`OC:index.ts:303-317`, `:922-926`) | `bash.detach_on_user_message` behaviour is already live on OC1. Its description text is not. |
| OpenCode 1 configure-warnings callback | `loadAftConfigOrLastGood(projectRoot)` (`OC:index.ts:350-362`) | `configure_warnings_delivery` is already live for that callback |
| Daemon standing actor tick | Actor config snapshots (`R:subc/standing.rs:106-126`, `:168-200`) | `index.roots` follows whatever the actor contexts last published |
| `aft index` CLI | Files on each run (`R:cli/index.rs:145-153`) | One-shot |
| OpenCode 2 Location reload | Whole `bootLocation` (`OC:entry/server-runtime.mjs:46-117`) | Full reload, including registration (trigger unverified) |

---

## 4. Per-key inventory

Column meanings:

- **When**: `call` = read from a fresh snapshot on each call; `cfg` = read inside configure; `ctor(X)` = captured by component X when it is built; `start` = plugin startup; `inert` = accepted but never read.
- **Live change needs**: what a watcher-driven re-resolve must do beyond publishing a new snapshot.
- **Prefix**: whether a change alters tool definitions or system text the model sees.

### 4.1 Edit pipeline

| Key | Tier | Readers (file:line) | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `$schema` | F | none (merge only, `R:config_resolve.rs:963-965`) | inert | nothing | no | — |
| `edit_mode` | F | Rust: `hashline_enabled` (`R:config_resolve.rs:1606`), `R:run_tool_call.rs:244` (call), `register_hashline_for_configure` (`R:commands/configure.rs:2589`, `:3413-3419`, cfg). OC: `openCodeHashlineEffective` (`OC:index.ts:332`) picks the edit/read schema arm; hints (`:822`); `edit_slot_survives` is write-once (`BR:pool.ts:424-440`). Pi: `piHashlineEffective` (`PI:tool-registration.ts:110-115`, `PI:index.ts:756`) | call (Rust), start (TS) | impossible without host restart (OC1, Pi); OC2 only on Location reload | **yes**: edit/read schema + description + hints | Half-applied is dangerous. The engine combines the configured flag with the host's write-once `edit_slot_survives` (`R:hashline/integration/binding.rs:80-84`). If only Rust flips, Rust and the registered edit schema disagree about argument shape. |
| `format_on_edit` | F | `R:format.rs:1529` (call); configure missing-tool warnings `R:commands/configure.rs:1963-1965` (cfg) | call | nothing | no | — |
| `formatter_timeout_secs` | U | `R:format.rs:1590` (call) | call | nothing | no | — |
| `type_checker_timeout_secs` (not in schema) | U (`R:config_resolve.rs:1487`) | `R:format.rs:2342` (call) | call | nothing | no | Missing from `assets/aft.schema.json` |
| `validate_on_edit` | F | `R:edit.rs:653`, `R:main.rs:1375` (call); `R:commands/configure.rs:1967-1969` (cfg warnings) | call | nothing | no | — |
| `formatter.<lang>` | F (map merge `R:config_resolve.rs:1082`) | `R:format.rs:791` (call); `R:commands/configure.rs:1779` (cfg) | call | clear the per-root tool-availability cache, as configure does (`R:commands/configure.rs:3276`, `:6142`) | no | Selects which external formatter binary runs. Only from a fixed enum. |
| `checker.<lang>` | F | `R:format.rs:936` (call); `R:commands/configure.rs:1875-1878` (cfg) | call | same as `formatter` | no | Same as `formatter` |
| `configure_warnings_delivery` | F | Rust: inert (not in `Config`, `R:config_resolve.rs:116`). OC: `OC:index.ts:350-362` (fresh re-read per callback), `:900` (startup snapshot). Pi: not read | TS per callback / start | swap TS snapshot | no | — |

### 4.2 Tool surface and path restriction

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `disabled_tools` | T (add-only; `aft_safety` and host slots protected, `R:config_resolve.rs:1166-1178`, `:1535-1548`) | Rust: `tool_enabled` (`R:context.rs:4631-4633`), used for steering text (`R:commands/zoom.rs:934`, `R:commands/semantic_search/mod.rs:5074`); `aft_search_registered` derived at `R:config_resolve.rs:1615` and read by the grep footer (`R:bash_rewrite/rules.rs:550`); `read_slot_survives` at cfg (`R:commands/configure.rs:3416`). OC: registration (`OC:index.ts:728-734`), bash description (`:806-816`), hints (`:817-822`), `toolEnabled` wording (`OC:tools/reading.ts:64`, `navigation.ts:31`, `bash.ts:394`). Pi: `resolvePiToolSurface` (`PI:tool-registration.ts:153-185`), hints (`PI:workflow-hints.ts:199-211`), wording (`PI:tools/bash.ts:488`, `reading.ts:353`, `navigate.ts:134`) | start (TS), call (Rust) | impossible without host restart (OC1; Pi except adding registrations); OC2 on Location reload | **yes**: registered tool set, descriptions, hints | Central to the pending "refuse disabled tools at call time" change. If Rust republishes live while the host keeps the startup registration, Rust refuses calls to tools the model can still see. |
| `restrict_to_project_root` | U | Rust: `path_restriction_context` (`R:context.rs:8743-8778`), `R:commands/semantic_search/mod.rs:687` (call). OC: `OC:tools/permissions.ts:441`, `:553` (call, startup snapshot). Pi: per call from `ctx.config` (`PI:tools/fs.ts:176`, `:245`, `navigate.ts:162`, `ast.ts:327`, `safety.ts:213`, `conflicts.ts:101`, `imports.ts:191`, `inspect.ts:99`); surface flag at start (`PI:tool-registration.ts:167`) used by hashline edit (`PI:tools/hoisted.ts:740`) | call (both, but TS uses the startup snapshot) | republish Rust **and** swap TS snapshot | no description effect found | **Security boundary.** Enforced on both sides. A Rust-only republish that turns it off leaves TS still blocking (fail-closed). Turning it on is enforced by Rust even if TS lags. |
| `url_fetch_allow_private` | U | `R:commands/zoom.rs:88`, `R:commands/outline.rs:212` (call) | call | nothing | no | SSRF surface. Configure's reset-onto-default protects it from cross-bind inheritance (`R:config_resolve.rs:752-761`). |

### 4.3 Indexes, views, callgraph, standing roots

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `indexes.trigram` | T (off only, `R:config_resolve.rs:1191-1211`) | warm key (`R:commands/configure.rs:2450`); `R:cli/warmup.rs:538`; health (`R:context.rs:3300-3310`) | cfg + call | rebuild: drop and reload the search index (`R:commands/configure.rs:3597-3600`) | no (registration is independent, `R:config.rs:370-372`) | Turning it off live discards the resident index |
| `indexes.semantic` | T | warm key; semantic lane (`R:commands/configure.rs:3461-3465`, `:3601-3643`); OC ONNX gate (`OC:bridge-bootstrap.ts:510-511`), `get-warnings` (`OC:index.ts:687-699`); Pi ONNX gate (`PI:index.ts:275-278`, `:492`) | cfg (Rust), start (TS) | rebuild the semantic index and embedding model | no | Turning it on live when ONNX was not prepared at startup: the plugin never downloaded the runtime, and configure only finds one that is already on disk (`R:commands/configure.rs:3245-3249`). The index then reports degraded or unavailable until restart. |
| `indexes.callgraph` | T | warm key and callgraph build key (`R:commands/configure.rs:2464-2483`); `R:context.rs:5733` (call); `R:inspect/manager.rs:881` (job snapshot) | cfg + call | rebuild the callgraph store (force rebuild `:3622-3636`) | no | Cold build cost on large roots |
| `index.roots[]` (not in schema) | U (`R:config_resolve.rs:1512-1519`) | daemon standing actor (`R:subc/standing.rs:132-137`, `:168-200`); health (`R:subc/health.rs:3922`); `aft index` CLI (`R:cli/index.rs:145-162`) | per standing tick (daemon) | already follows the republished actor config; reconcile creates or retires standing actors | no | Daemon only. A repository must not mint standing roots (user-only). |
| `views.enabled` (not in schema) | F | warm key; maintenance `ViewLoad` (`R:commands/configure.rs:6232`); `R:context.rs:5036`, `:5243`, `:5733` (call) | cfg + call | rebuild: open or clear the view runtime | no | — |
| `callgraph_chunk_size` | F | `R:context.rs:5628`, `:5636`, `:6136` (at cold build); warm key (`R:commands/configure.rs:2456`) | ctor(callgraph cold build) | only affects the next cold build. Because it is in the warm key, today's configure path reloads artifacts anyway. | no | — |

### 4.4 Inspect

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `inspect.enabled` | F | `R:main.rs:1046` (call gate); `R:context.rs:7209` (tier-2 start); warm key (`R:commands/configure.rs:2457`) | call | republish. Warm key also resets the tier-2 scheduler (`:3487`). | no (registration is `disabled_tools`) | — |
| `inspect.diagnostics_timeout_ms` | T (project may raise only, `R:config_resolve.rs:1409-1419`) | `R:inspect/diagnostics_category.rs:22` (call); OC transport timeout `OC:tools/inspect.ts:348` (startup snapshot); Pi `PI:tools/inspect.ts:480` | call | republish + swap TS snapshot | no | If only Rust changes, the TS transport timeout can be shorter than the Rust wait |
| `inspect.tier2_idle_minutes` | F | Rust: none (absent from `InspectConfig`, `R:config.rs:290-301`). OC: closure over startup config `OC:index.ts:829`. Pi: none | TS start | swap TS snapshot | no | — |
| `inspect.tier2_pass_timeout_ms` | F | `R:inspect/manager.rs:2319` (per tier-2 job snapshot) | per job | nothing | no | — |
| `inspect.categories` | F | none | inert | nothing | no | Parsed but unused |
| `inspect.tier2_soft_deadline_ms` | F | none | inert | nothing | no | Parsed but unused |
| `inspect.max_drill_down_items` | F | none | inert | nothing | no | Parsed but unused |
| `inspect.duplicates.expected_mirrors` | F | `R:inspect/manager.rs:5035`, `R:inspect/cache.rs:2103`, `R:inspect/scanners/duplicates.rs:159` (per job) | per job | nothing (cached aggregates update on the next pass) | no | — |

### 4.5 Idle, worktree, backup

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `idle.root_ttl_minutes` | F | daemon reaper `R:subc/mod.rs:1346-1351` (per tick) | per tick (daemon only) | nothing | no | No standalone reader |
| `idle.lsp_ttl_minutes` | F | `R:runtime_drain.rs:3363`, `:3388` (per drain) | per drain | nothing | no | — |
| `worktree.ram_overlay` | F | `R:context.rs:4930-4932` (queried by watcher arms) | call | republish. Not verified whether edits made while it was off get replayed when it turns on. | no | Read-only linked worktrees only; never persists |
| `backup.enabled` | U (`R:config_resolve.rs:1499-1505`) | Rust: `BackupStore::set_policy` at cfg (`R:commands/configure.rs:3423-3436`); dispatch gates `R:main.rs:1072`, `:1087` (call). OC descriptions `OC:tools/hoisted.ts:551`, `:684`, `:1008`, `:1149`, `:1253`, `OC:tools/imports.ts:20`. Pi `PI:tools/hoisted.ts:627-629`, `PI:tools/fs.ts:142`, `PI:tools/imports.ts:170` | cfg + call (Rust), start (TS) | Rust: republish + `set_policy`. TS descriptions: host restart. | **yes** (descriptions) | Turning it off live removes undo protection for edits after the switch |
| `backup.max_depth` | U | `set_policy` at cfg (`R:commands/configure.rs:3428`); `R:backup.rs:522-533` | ctor(backup store policy) | push `set_policy` | no | Lowering it **prunes existing undo history** on memory and disk (`R:backup.rs:522-533`) |
| `backup.max_file_size` | F (the only project-settable backup leaf, `R:config_resolve.rs:1100-1110`) | `set_policy` at cfg (`R:commands/configure.rs:3429-3434`) | ctor(backup store policy) | push `set_policy` | no | — |

### 4.6 Sandbox

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `sandbox.enabled` | T (project may enable, never disable, `R:config_resolve.rs:1128-1133`, `:1523-1525`) | `R:sandbox_spawn.rs:995`, `:999`, `:1036`; `R:bash_background/mod.rs:226`; `R:commands/bash.rs:180` (per spawn) | per spawn | nothing. Already-running tasks keep their spawn-time confinement. | not found | **Security.** A live change has a mixed-state window: tasks spawned before the change run under the old policy. |
| `sandbox.write_allow` | U | `R:sandbox_spawn.rs:1247-1253` (per spawn) | per spawn | nothing | not found | **Security** (widens writable roots) |
| `sandbox.read_deny` | T (union, `R:config_resolve.rs:1120-1127`) | `R:sandbox_spawn.rs:1284-1290` (per spawn) | per spawn | nothing | not found | **Security.** Note: the sandbox itself denies children read and write access to `~/.config/cortexkit` and write access to `<root>/.cortexkit` (`R:sandbox_spawn.rs:1255-1273`), so a sandboxed agent shell cannot edit these config files. |

### 4.7 Bash runtime (`bash` bool/object, legacy `experimental.bash.*`)

Rust resolution: `R:config_resolve.rs:2099-2226`. TS resolution: `resolveBashConfig` (`OC:config.ts:941-1035`, `PI:config.ts:493-587`). Where top-level `bash` is absent, `experimental.bash.{rewrite,compress,background,long_running_reminder_*}` feeds the same fields (`R:config_resolve.rs:2200-2215`; TS legacy branch in `resolveBashConfig`). The legacy keys have the same rows as their top-level twins.

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `bash.enabled` | F | `R:main.rs:1038` (call gate) | call | nothing | no (never removes a registration, `PI:tool-registration.ts:238-239`) | — |
| `bash.rewrite` | F | `R:bash_rewrite/dispatch.rs:94`, `R:commands/read.rs:1078` (call) | call | nothing | no | — |
| `bash.compress` | F | Rust: `R:compress/mod.rs:363` (call); atomic mirror synced at configure (`R:context.rs:3802-3806`, `R:commands/configure.rs:6273`) and seeded at context construction (`R:context.rs:2972`). OC: bash description `OC:index.ts:806-816`, hints. Pi: `PI:tools/bash.ts:496-498`, hints `PI:workflow-hints.ts:163` | call (Rust), start (TS) | Rust: republish + `sync_bash_compress_flag`. TS text: host restart. | **yes** (bash description, hints) | — |
| `bash.background` | F | Rust: `R:bash_background/mod.rs:167` (call). OC: schema params and description `OC:tools/bash.ts:313-381`, per call `:398`. Pi: schema, description, snippet `PI:tools/bash.ts:502-523`, per call `:528`; hints on both hosts | call (Rust), start (TS schema) | Rust republish; TS schema: host restart | **yes** (schema, description, hints) | Half-applied: Rust off while the schema still offers `background`/`pty` means calls fail with `feature_disabled` |
| `bash.host_fallback` | F | Rust: inert (`R:config.rs:490-492`). OC `OC:tools/bash.ts:520` (call). Pi `PI:tools/bash.ts:645-647` (call) | call (TS snapshot) | swap TS snapshot | no | **Security**: break-glass host execution. The schema says it is project-settable, and every fallback still requires a fresh host permission prompt. |
| `bash.subagent_background` | F | Rust: inert (`R:config_resolve.rs:2101-2103`). OC `OC:tools/bash.ts:438`, `OC:tools/bash_watch.ts:97`. Pi `PI:tools/bash.ts:560` | call (TS snapshot) | swap TS snapshot | no | — |
| `bash.detach_on_user_message` | F | Rust: inert. OC description `OC:index.ts:809-816`, `OC:tools/bash.ts:170-171`, `:365`; behaviour per message with **fresh re-read** `OC:index.ts:922-926` → `OC:bash-wait-detach.ts:71-73`. Pi description `PI:tools/bash.ts:499-501`; behaviour per input from the startup config `PI:index.ts:996` | per message | behaviour: already live on OC1; swap snapshot on Pi. Text: host restart. | **yes** (bash and `wait` descriptions) | On OC1 the behaviour and the description can already disagree after an edit |
| `bash.watch_sync_max_ms` | F | Rust: inert (resolved and clamped only, `R:config_resolve.rs:1877-1897`). OC `OC:tools/bash_watch.ts:133`, `OC:tools/bash.ts:564`. Pi `PI:tools/bash.ts:804`, `:869` | call (TS snapshot) | swap TS snapshot | no (descriptions name the key, not its value) | — |
| `bash.linux_scope` | U (`R:config_resolve.rs:1313-1321`, `:1530-1534`) | `R:bash_background/mod.rs:213-215` (per spawn) | per spawn | nothing | no | Linux only |
| `bash.long_running_reminder_enabled` / `_interval_ms` | F | pushed into `BgTaskRegistry` at configure maintenance (`R:commands/configure.rs:6171-6174` → `R:bash_background/registry.rs:1795`) | ctor-like push (cfg) | republish + call `configure_long_running_reminders` | no | — |
| `bash.foreground_wait_window_ms` | F | Rust `R:subc/bash.rs:552`, `R:commands/bash_orchestrate.rs:152` (call). OC `OC:tools/bash.ts:455`; Pi `PI:tools/bash.ts:527` | call | republish + swap TS snapshot | no | — |
| `bash.powershell_tool` | F | Rust: inert. Pi registration fallback `PI:tool-registration.ts:67-69`, `:156-162`, re-checked once at `session_start` `PI:index.ts:896-911`. OpenCode never registers PowerShell | start (Pi) | Pi can add the registration at runtime (precedent at `PI:index.ts:896-911`). Removing it needs a host restart. | **yes** (Pi tool set) | — |

### 4.8 LSP

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `lsp.servers.<id>.{extensions,binary,args,root_markers,disabled,env,initialization_options}` | U (`R:config_resolve.rs:1571-1573`) | `resolved_servers` via `servers_for_file` (`R:lsp/registry.rs:700-760`), called per file/diagnostic request (`R:inspect/diagnostics_category.rs:201`, `:274`); configure missing-binary warnings (`R:commands/configure.rs:2107`) | call (registry), ctor(LSP process) | rebuild: new spawns see new definitions right away, but **already-running server processes keep the old binary, args, env and init options**. Configure never restarts them (only `set_search_paths` and `clear_failed_spawns`, `R:commands/configure.rs:2970-2976`, `:3408-3409`, `:6283`). They exit on idle TTL. Applying cleanly means restarting the affected servers, which loses open-document state and cached diagnostics. | no | **Security**: arbitrary executable plus environment. User-only because a repository must not pick an executable. |
| `lsp.disabled` | U | `R:lsp/registry.rs:708` (call); compared specially in `configs_equal_including_runtime_only_fields` (`R:commands/configure.rs:1417-1424`). TS: auto-installer at start (`OC:bridge-bootstrap.ts:208`, `PI:index.ts:527`) | call (Rust), start (TS) | rebuild: stop now-disabled running servers | no | Also suppresses diagnostics (user-only rationale, `R:config_resolve.rs:45-46`) |
| `lsp.python` | F | mapped to `experimental_lsp_ty` / `disabled_lsp` (`R:config_resolve.rs:2032-2042`) | call | rebuild (swap running Python server) | no | — |
| `experimental.lsp_ty` | F | `R:lsp/registry.rs:710` (call) | call | same as `lsp.python` | no | — |
| `lsp.diagnostics_on_edit` | F | `R:commands/tool_call.rs:82` (call); daemon `RootMeta` captured at RouteBind (`R:subc/mod.rs:5642-5647`, `:5203-5208`) and read per call (`:6425-6428`) | call; bind (daemon) | republish **and** update the daemon `RootMeta` | no | Daemon-only split state |
| `lsp.auto_install` | U | Rust: not in `Config`; plugin sends `lsp_auto_install_binaries` instead (`OC:bridge-bootstrap.ts:205`, `:215-221`; `PI:index.ts:524-541`) | start (TS auto-install pass) | re-run the plugin's auto-install pass (a TS component) or restart | no | Downloads and runs third-party binaries |
| `lsp.grace_days` | U | `OC:bridge-bootstrap.ts:206`; `PI:index.ts:525` | start | same as `auto_install` | no | Supply-chain window guard |
| `lsp.versions` | U | `OC:bridge-bootstrap.ts:207`; `PI:index.ts:526` | start | same as `auto_install` | no | Version pins bypass the grace filter |

### 4.9 Semantic search

`semantic` is part of the warm key as a whole struct (`R:commands/configure.rs:2453`). Today, changing any `semantic.*` leaf through configure takes the non-equivalent path, which drops and reloads artifacts. The semantic index itself is rebuilt only when `semantic_build_inputs_changed` is true: backend, model, base URL, subc connection, `max_files` or the index switch changed (`R:commands/configure.rs:1396-1404`, `:3461-3465`).

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `semantic.backend` | U (`R:config_resolve.rs:1551-1553`) | fingerprint (`R:commands/configure.rs:1400`); `R:semantic_index.rs:224`; `R:feature_status.rs:285`; per query clone `R:commands/semantic_search/mod.rs:1780`; TS ONNX gate at start (`OC:bridge-bootstrap.ts:510`, `PI:index.ts:275-278`) | ctor(embedding model, semantic index) | rebuild the semantic index and model; embeddings are discarded | no | **Security**: endpoint and credential selection. Switching to fastembed live when ONNX was not prepared hits the same gap as `indexes.semantic`. |
| `semantic.model` | F | fingerprint (`:1401`) | ctor | rebuild the index | no | — |
| `semantic.base_url` | U | fingerprint (`:1402`) | ctor | rebuild the index | no | **Security** (endpoint; project value dropped) |
| `semantic.api_key_env` | U | read when the embedding model is built. The cached model is cleared only when a semantic build is not adopted (`R:commands/configure.rs:3637-3643`). Not part of the fingerprint. | ctor(embedding model) | rebuild the model. Whether a key-only change actually rebuilds the cached model through today's configure was **not verified**. | no | **Security** (credential name) |
| `semantic.timeout_ms` | F | build-time HTTP floor (`R:config.rs:203-206`) | ctor(build worker) | applies to the next build | no | — |
| `semantic.query_timeout_ms` | U (`R:config_resolve.rs:1565-1567`) | per query (cloned config `R:commands/semantic_search/mod.rs:1780`, `:4613`) | call | nothing in principle. Via configure it is a warm-key change. | no | — |
| `semantic.query_instruction` | F | per query (`resolved_query_instruction`, `R:config.rs:252-264`) | call | same as `query_timeout_ms` | no | — |
| `semantic.max_input_tokens` | F | chunk construction at build (`R:config.rs:216-219`) | ctor(build) | rebuild to apply to existing chunks | no | — |
| `semantic.max_batch_size` | F | build batching (`R:cli/index.rs:485`; runtime build via cloned config `R:runtime_drain.rs:1087`) | ctor(build) | applies to the next build | no | — |
| `semantic.max_files` | F | build input (`R:commands/configure.rs:3465`; `R:cli/index.rs:291`) | ctor(build) | rebuild | no | Memory guard for local fastembed |

### 4.10 Transport, integrations, host

| Key | Tier | Readers | When | Live change needs | Prefix | Risk notes |
|---|---|---|---|---|---|---|
| `bridge.request_timeout_ms` | U | TS only: `resolveBridgePoolTransportOptions` (`OC:config.ts:1897-1906`, `PI:config.ts:1899-1908`) captured by the pool at creation (`OC:index.ts:339`, `PI:index.ts:642`, `OC:entry/server-runtime.mjs:85`). Rust: raw only (`R:config_resolve.rs:141`) | ctor(bridge pool) | rebuild the pool (no setter found), which restarts bridges and loses in-flight requests | no | Bridge safety and restart budget |
| `bridge.hang_threshold` | U | same | ctor(bridge pool) | same | no | same |
| `subc.connection_file` | U | TS: transport selection and existence check at start (`OC:bridge-bootstrap.ts:414`, `:486`; `OC:index.ts:411-416`; `PI:config-error-state.ts:107`; `PI:index.ts:430`, `:711`). Rust: resolved into `semantic.subc_connection_file` (`R:config_resolve.rs:1770-1774`) for the Synapse backend (`R:synapse_embed.rs:190`) and the semantic fingerprint (`R:commands/configure.rs:1403`) | start (TS), ctor (Rust) | impossible without host restart (transport choice) | no | Selects daemon versus standalone. A missing file is a config error state (`OC:bridge-bootstrap.ts:413-415`). |
| `github.shim` | U | Rust: `agent_child_env::maintain` on the full configure path (`R:commands/configure.rs:3238` → `R:agent_child_env.rs:263-287`) installs or removes the shim entry under the process storage root; `inject` per child spawn (`R:agent_child_env.rs:401-422`, from `R:bash_background/mod.rs:204`) | cfg + per spawn | rebuild: re-run `maintain` | not found | **Security.** The shims directory is per process, so in the daemon it is shared by all roots. A user `harnesses.<id>.github` override that differs between binds flips one shared entry for every root. |
| `github.read` | U | Rust `R:github_read/cache.rs:377` (call). OC `OC:tools/hoisted.ts:397`, `OC:tools/reading.ts:65` (descriptions). Pi `PI:tools/hoisted.ts:546`, `PI:tools/reading.ts:354-360` | call (Rust), start (TS) | Rust republish; TS text: host restart | **yes** (`issue://`/`pr://` wording in read/outline/zoom) | Comment `OC:config.ts:1775-1777`: user-only partly to keep prefix caches stable |
| `github.write` | U (implies read, `R:config_resolve.rs:1975-1987`) | Rust `R:commands/github_comments.rs:361` (call). OC descriptions `OC:tools/hoisted.ts:554`, `:687`; per call `:576`, `:898`. Pi descriptions `PI:tools/hoisted.ts:630`, `:762`; per call `:658`, `:818` | call + start | as `github.read` | **yes** | Write capability |
| `gh_shim.binary_path` | U | `shim_binary` inside `maintain` (`R:agent_child_env.rs:524-548`) | cfg (full path only) | re-run `maintain` | no | **Security** (executable origin; self-referential pins rejected, `:549-583`) |
| `git.co_author` | F | `maintain` installs managed hooks when not `off` (`R:agent_child_env.rs:289-291`); `inject` per spawn (`:422`, `:512-513`) | cfg + per spawn | re-run `maintain` when turning on (hooks) | no | Commit metadata only |
| `pi.tool_presentation` | F | Pi only: `bindToolRegistrationFunnel` (`PI:tool-registration.ts:203`) → OMP `loadMode` (`PI:tools/_shared.ts:130-165`) | start | host restart (Pi/OMP) | **yes** | — |
| `auto_update` | U | OC only: `createAutoUpdateCheckerHook` (`OC:index.ts:745-753`). Pi strips it as OpenCode-only (`PI:__tests__/config.test.ts:1088-1118`) | ctor(auto-update hook) | rebuild the hook, or restart | no | Suppressing security updates (user-only) |
| `harnesses.<id>.*` | tier of its file | selection at resolve (`R:config_resolve.rs:899-957`; `OC:config.ts:670-682`; `PI:config.ts:42-74`) | as the inner key | as the inner key | as the inner key | Daemon: different harnesses on one root reset the shared root config on each bind (`R:config_resolve.rs:752-761`), so a live reload must re-resolve per active bind identity |

Process-state configure parameters (`storage_dir`, `lsp_paths_extra`, `lsp_auto_install_binaries`, `lsp_inflight_installs`, `bash_permissions`, `edit_slot_survives`) are not `aft.jsonc` keys. They are carried across resolves (`R:config_resolve.rs:833-847`) and are out of scope.

---

## 5. Watcher facts

### 5.1 `<root>/.cortexkit/aft.jsonc` and the project watcher

- **Setup.** The project watcher is started per root by configure maintenance (`R:commands/configure.rs:6184-6216`) and reattached on demand after idle eviction (`ensure_project_watcher`, `R:commands/configure.rs:774`). It is recursive on the root:
  - generic backend: `R:watcher_backend/mod.rs:53`;
  - macOS: FSEvents with exact-path exclusions (`R:watcher_backend/mod.rs:21-36`);
  - Linux: one inotify watch per directory that never descends into excluded or ignored directories (`R:watcher_backend/inotify.rs:24-60`, `R:watcher_backend/mod.rs:24-36`).
- **Filtering.** `.cortexkit` is **not** on the hard-coded infra skip list (`.git`, `.opencode`, `.alfonso`, `.gsd`, `node_modules`, `target`; `R:watcher_filter.rs:237-244`, `:259-266`). A change to `.cortexkit/aft.jsonc` therefore reaches the changed-path set (`R:watcher_filter.rs:1344-1365`), **unless** the project's gitignore/aftignore matches it (`R:watcher_filter.rs:328-337`). On Linux an ignored `.cortexkit/` never gets an inotify watch at all. Ignoring `.cortexkit/*` is a realistic layout: the watcher's own tests use exactly that gitignore (`R:watcher_filter.rs:2555`).
- **What happens today.** The event is treated as an ordinary corpus file. The search-index replay updates or removes it (`R:runtime_drain.rs:649-682`), and tier-2 and other lanes see a changed path. No code in the watcher filter or drain handles `aft.jsonc` specially, and nothing re-runs configure.
- **When there is no watcher at all:**
  - a HOME root (`if !job.home_match`, `R:commands/configure.rs:6184`, `:6214`);
  - an idle-evicted root until its next request;
  - test runs with `AFT_TEST_DISABLE_FILE_WATCHER=1` (`R:commands/configure.rs:738-740`).
- **Conclusion.** Reusing the project watcher is free when `.cortexkit` is not ignored, but it cannot be relied on in general. A config watch needs either an explicit exemption for `.cortexkit/aft.jsonc` in the filter (Linux would still need a watch placed there) or its own non-recursive watch on `<root>/.cortexkit/`. The macOS backend already creates a separate non-recursive watcher for extra paths (`R:watcher_backend/fsevents.rs:56-69`), and the generic backend adds extra non-recursive paths (`R:watcher_backend/mod.rs:54-58`).

### 5.2 `~/.config/cortexkit/aft.jsonc`

Nothing watches it in Rust or TS. The daemon resolves the path once (`R:main.rs:236`). Standalone engines get it per configure (`R:commands/configure.rs:2362-2380`), and the plugins compute it in `buildConfigTierConfigureParams`. The one in-repo precedent for watching a user-level file is the TUI preferences watcher. It watches the **parent directory**, filters by file name including the `.tmp` sibling of an atomic rename, and debounces by 150 ms (`OC:tui/preferences.ts:200-216`). The same shape suits `aft.jsonc`, because editors replace the file by rename.

---

## 6. Cost of watching: daemon versus standalone

| | Daemon (subc) | Standalone |
|---|---|---|
| Processes | One `aft` process with many roots (one actor `AppContext` per root) | One `aft` process per project root per host process (one bridge per root in `BR:pool.ts`) |
| User file | One non-recursive watch on `~/.config/cortexkit/` for the whole daemon. On change, re-resolve every live actor (each has its own project tier and bind harness). | One watch per `aft` process, so K bridges mean K watches of the same file. Each host plugin process also needs its own watch for TS-owned keys and `ctx.config`. |
| Project file | Covered by each root's existing watcher when not ignored, otherwise one extra non-recursive watch per root | Same, per process |
| Who applies Rust keys | The daemon itself: it already reads files at every RouteBind (`R:subc/mod.rs:5631`) | Either the engine re-reads by itself, or the plugin drives it: `BridgePool.reconfigure` already re-sends configure to a live bridge (`BR:pool.ts:457-478`), and configure re-reads both files from disk (`R:commands/configure.rs:2389-2409`) |
| Who applies TS keys | Each host plugin process, since the daemon cannot reach TS snapshots | Each host plugin process |
| Extra state | `RootMeta.diagnostics_on_edit` (`R:subc/mod.rs:5203-5208`); standing actor already follows actor snapshots (`R:subc/standing.rs:168-170`) | none beyond the root context |

The watch itself costs one inotify descriptor or one FSEvents stream per watcher. The real cost is what a re-resolve triggers:

- a warm-key change drops and reloads artifacts (`R:commands/configure.rs:3560-3664`);
- the full configure path needs exclusive use of the root (`:3223`), so it waits for in-flight requests on that root;
- in the daemon, one user-file edit fans out to every live root.

---

## 7. Proposed grouping (proposal for the operator)

**This is a proposal.** The operator decides per key.

**A: safe to apply live by republishing a snapshot.** Where marked `+TS`, the plugin must also swap its `ctx.config`. Where marked `+push`, an existing setter must be called on a live component.

- `$schema`, `format_on_edit`, `formatter_timeout_secs`, `type_checker_timeout_secs`, `validate_on_edit`, `formatter`, `checker` (+ clear the format tool cache)
- `configure_warnings_delivery` (+TS)
- `url_fetch_allow_private`
- `restrict_to_project_root` (+TS; security: republish both sides together)
- `inspect.enabled`, `inspect.diagnostics_timeout_ms` (+TS), `inspect.tier2_idle_minutes` (+TS), `inspect.tier2_pass_timeout_ms`, `inspect.duplicates.expected_mirrors`
- inert keys: `inspect.categories`, `inspect.tier2_soft_deadline_ms`, `inspect.max_drill_down_items`
- `idle.root_ttl_minutes`, `idle.lsp_ttl_minutes`, `worktree.ram_overlay`
- `backup.max_file_size` (+push `set_policy`)
- `sandbox.enabled`, `sandbox.write_allow`, `sandbox.read_deny` (take effect at the next spawn; security review of the mixed-state window)
- `bash.enabled`, `bash.rewrite`, `bash.linux_scope`, `bash.foreground_wait_window_ms` (+TS)
- `bash.host_fallback`, `bash.subagent_background`, `bash.watch_sync_max_ms` (+TS)
- `bash.long_running_reminder_*` (+push `configure_long_running_reminders`)
- `lsp.diagnostics_on_edit` (+daemon `RootMeta`)
- `semantic.query_timeout_ms`, `semantic.query_instruction`, `callgraph_chunk_size`. These are safe by read site, but today's configure treats them as warm-key changes and reloads artifacts.
- `git.co_author` (+push `maintain` when turning on)

**B: live, but requires rebuilding or restarting a component.**

- `indexes.trigram` / `indexes.semantic` / `indexes.callgraph`: drop and rebuild or reload the index. The resident index is lost. Semantic-on also depends on ONNX being present.
- `views.enabled`: view runtime.
- `index.roots`: standing roots reconcile (daemon).
- `semantic.backend`, `semantic.model`, `semantic.base_url`, `semantic.api_key_env`, `semantic.max_files`, `semantic.max_input_tokens`, `semantic.timeout_ms`, `semantic.max_batch_size`: semantic index and embedding model. Embeddings are lost.
- `lsp.servers.*`, `lsp.disabled`, `lsp.python`, `experimental.lsp_ty`: restart affected LSP servers. Open documents and diagnostics are lost.
- `lsp.auto_install`, `lsp.grace_days`, `lsp.versions`: re-run the plugin auto-install pass.
- `backup.max_depth` (`set_policy`; lowering prunes history).
- `github.shim`, `gh_shim.binary_path`: re-run `agent_child_env::maintain`. The shims directory is shared per process.
- `auto_update`: auto-update hook.
- `bridge.request_timeout_ms`, `bridge.hang_threshold`: bridge pool. There is no setter, so today this means restarting bridges; see C.

**C: restart only (the prefix changes, or the choice is fixed at host load).**

- `disabled_tools`, `edit_mode`: tool set and schema; OC1, Pi. OC2 applies them on a Location reload.
- `bash.background`, `bash.compress`, `bash.detach_on_user_message`: schema, description and hints. The Rust-side and behavioural parts could go live, but that splits behaviour from text.
- `backup.enabled`, `github.read`, `github.write`: description text. The Rust gates could go live, with the same split.
- `bash.powershell_tool` (Pi can add but not remove), `pi.tool_presentation`.
- `subc.connection_file`: transport.
- `bridge.*` unless the pool gains a live setter.
- legacy `experimental.bash.{rewrite,compress,background}`: same as their top-level twins.

---

## 8. Method and limits

- The checklist was `assets/aft.schema.json` plus `RawAftConfig` and nested raw structs (`R:config_resolve.rs:99-610`). Read sites were found by grep and by reading the resolver, configure, context, sandbox, agent-environment, LSP registry, watcher and subc files. Rust line numbers were spot-checked against the worktree. TS line numbers come from reading and grepping `OC:`/`PI:`/`BR:` sources.
- **Not verified:**
  - what triggers an OpenCode 2 Location reload;
  - whether `worktree.ram_overlay` replays edits missed while it was off;
  - whether a change to only `semantic.api_key_env` rebuilds the cached embedding model through today's configure;
  - whether any OpenCode 2 tool text differs from OpenCode 1 for bash (the V2 entry does not call the bash description override; `OC:entry/server-runtime.mjs:99-116`);
  - Pi host APIs beyond what the plugin calls.
- Where a row says "not found", the search covered the obvious read sites but was not an exhaustive proof.
