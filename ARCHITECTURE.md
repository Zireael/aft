# Architecture

## Pattern Overview

**Overall:** TypeScript plugin + Rust worker process over a session-scoped NDJSON bridge

**Key Characteristics:**
- Use `packages/opencode-plugin/src/index.ts` to register OpenCode tools and map them onto Rust commands.
- Use `packages/aft-bridge/src/bridge.ts` and `packages/aft-bridge/src/pool.ts` to isolate one `aft` process per session. Both harness adapters (OpenCode, Pi) import these shared primitives from `@cortexkit/aft-bridge`.
- Use `crates/aft/src/commands/` handlers to keep protocol dispatch thin and command logic modular.
- Use `crates/aft/src/edit.rs`, `crates/aft/src/format.rs`, `crates/aft/src/callgraph.rs`, and `crates/aft/src/lsp/` as shared engines behind multiple commands.

## Layers

**OpenCode integration layer:**
- Purpose: Register tools, load config, and attach post-execution metadata.
- Location: `packages/opencode-plugin/src/index.ts`
- Contains: Plugin bootstrap, tool-surface selection, hoisting logic, disabled-tool filtering
- Depends on: `packages/opencode-plugin/src/config.ts`, `packages/opencode-plugin/src/tools/*.ts`, `packages/aft-bridge/src/pool.ts`
- Used by: OpenCode plugin loading through `@cortexkit/aft-opencode`

**Plugin transport layer:**
- Purpose: Resolve or download the binary, start worker processes, and forward requests.
- Location: `packages/aft-bridge/src/bridge.ts`, `packages/aft-bridge/src/pool.ts`, `packages/aft-bridge/src/resolver.ts`, `packages/aft-bridge/src/downloader.ts`
- Contains: Session bridge lifecycle, restart handling, version checks, binary discovery and download, ONNX runtime helpers, URL fetch
- Depends on: Node child-process APIs, GitHub releases, per-host logger adapters (via `setActiveLogger`)
- Used by: `packages/opencode-plugin/src/index.ts` and `packages/pi-plugin/src/index.ts` (both import from `@cortexkit/aft-bridge`)

**Tool definition layer:**
- Purpose: Convert OpenCode tool arguments into protocol requests and permission checks.
- Location: `packages/opencode-plugin/src/tools/`
- Contains: Hoisted tools, reading tools, import tools, transform tools, navigation tools, refactoring tools, safety tools, conflict tools, permissions helpers
- Depends on: `packages/aft-bridge/src/pool.ts`, `packages/opencode-plugin/src/metadata-store.ts`, `packages/opencode-plugin/src/lsp.ts`
- Used by: `packages/opencode-plugin/src/index.ts`

**Protocol and command layer:**
- Purpose: Accept NDJSON requests and route each command to a focused handler.
- Location: `crates/aft/src/main.rs`, `crates/aft/src/protocol.rs`, `crates/aft/src/commands/`
- Contains: Request dispatch, response encoding, command handlers for read/edit/refactor/LSP/conflicts/semantic search/bash
- Depends on: `crates/aft/src/context.rs`, `crates/aft/src/parser.rs`, `crates/aft/src/callgraph.rs`, `crates/aft/src/edit.rs`, `crates/aft/src/semantic_index.rs`, `crates/aft/src/compress/`
- Used by: `packages/aft-bridge/src/bridge.ts`

**Analysis and mutation engine layer:**
- Purpose: Parse code, compute call graphs, apply edits, format files, and manage imports.
- Location: `crates/aft/src/parser.rs`, `crates/aft/src/callgraph.rs`, `crates/aft/src/edit.rs`, `crates/aft/src/format.rs`, `crates/aft/src/imports.rs`, `crates/aft/src/extract.rs`
- Contains: Tree-sitter parsing, symbol extraction, diff generation, formatter detection, type-checker integration, refactor helpers
- Depends on: tree-sitter grammars, ast-grep, external formatter and checker processes
- Used by: `crates/aft/src/commands/*.rs`

**Semantic search engine layer:**
- Purpose: Embed, chunk, index, search, and rerank code by meaning across multiple backends.
- Location: `crates/aft/src/semantic_index.rs`, `crates/aft/src/vector_store.rs`, `crates/aft/src/semantic_rerank.rs`, `crates/aft/src/semantic_diagnostics.rs`, `crates/aft/src/semantic_doctor.rs`, `crates/aft/src/semantic_eval.rs`
- Contains: Multi-backend embedding engine (Fastembed, OpenAI-compatible, Ollama, Perplexity, Model2Vec), chunking strategies, vector storage traits, LLM-based reranking, search quality telemetry, health reports, local retrieval evaluation
- Depends on: `fastembed`, `model2vec-rs` (optional), `reqwest`, `tree-sitter`, `rayon`
- Used by: `crates/aft/src/commands/semantic_search.rs`, `crates/aft/src/commands/semantic_doctor.rs`, `crates/aft/src/commands/semantic_eval.rs`, `crates/aft/src/commands/configure.rs`

**Bash task management layer:**
- Purpose: Run, buffer, persist, and watchdog background shell tasks.
- Location: `crates/aft/src/bash_background/` (mod.rs, buffer.rs, process.rs, pty_process.rs, pty_runtime.rs, registry.rs, watchdog.rs, persistence.rs)
- Contains: Process lifecycle, PTY terminal emulation, output buffering, persistence to SQLite, watchdog timeout monitoring, task registry
- Depends on: `rusqlite`, PTY libraries, `crates/aft/src/db/`
- Used by: `crates/aft/src/commands/bash.rs`, `crates/aft/src/commands/bash_status.rs`, `crates/aft/src/commands/bash_kill.rs`, `crates/aft/src/commands/bash_promote.rs`, `crates/aft/src/commands/bash_drain_completions.rs`, `crates/aft/src/commands/bash_write.rs`

**Persistent storage layer:**
- Purpose: Store durable state (backups, bash task records, compression events) in SQLite.
- Location: `crates/aft/src/db/` (mod.rs, state.rs, backups.rs, bash_tasks.rs, compression_events.rs)
- Contains: SQLite schema management, backup/snapshot records, bash task archives, compression event log
- Depends on: `rusqlite`
- Used by: `crates/aft/src/bash_background/`, `crates/aft/src/backup.rs`, `crates/aft/src/compress/`

**State and diagnostics layer:**
- Purpose: Hold per-process mutable state for backups, checkpoints, file watching, call graph cache, and LSP state.
- Location: `crates/aft/src/context.rs`, `crates/aft/src/backup.rs`, `crates/aft/src/checkpoint.rs`, `crates/aft/src/lsp/`
- Contains: `AppContext`, undo history, named checkpoints, watcher receiver, LSP manager, diagnostics store, document store
- Depends on: `notify`, LSP transport helpers, Rust `RefCell`
- Used by: All command handlers through `AppContext`

## Data Flow

**Tool invocation flow:**

1. Register tool definitions and config-driven surface selection — `packages/opencode-plugin/src/index.ts`
2. Get a session bridge and send a command over NDJSON — `packages/aft-bridge/src/pool.ts`, `packages/aft-bridge/src/bridge.ts`
3. Dispatch the request to a Rust handler and return structured JSON — `crates/aft/src/main.rs`, `crates/aft/src/commands/mod.rs`

**Edit pipeline:**

1. Validate permissions and map tool arguments to protocol params — `packages/opencode-plugin/src/tools/hoisted.ts`, `packages/opencode-plugin/src/tools/permissions.ts`
2. Snapshot, mutate, diff, and validate content — `crates/aft/src/edit.rs`
3. Auto-format and optionally collect diagnostics after write — `crates/aft/src/format.rs`, `crates/aft/src/context.rs`

**Call-graph and navigation flow:**

1. Configure project root and initialize file watching — `crates/aft/src/commands/configure.rs`
2. Build or query lazy file-level graph data — `crates/aft/src/callgraph.rs`
3. Serve navigation commands such as callers, impact, and trace-data — `crates/aft/src/commands/callers.rs`, `crates/aft/src/commands/impact.rs`, `crates/aft/src/commands/trace_data.rs`

**Semantic search flow:**

1. Configure backend and embedding model — `crates/aft/src/commands/configure.rs`, `crates/aft/src/config.rs`
2. Chunk source files and embed content into a vector index — `crates/aft/src/semantic_index.rs`
3. Store vectors via the trait abstraction — `crates/aft/src/vector_store.rs`
4. Accept queries, embed, search, and optionally rerank — `crates/aft/src/commands/semantic_search.rs`, `crates/aft/src/semantic_rerank.rs`
5. Report index health and search quality — `crates/aft/src/commands/semantic_doctor.rs`, `crates/aft/src/semantic_diagnostics.rs`
6. Evaluate retrieval quality locally — `crates/aft/src/commands/semantic_eval.rs`, `crates/aft/src/semantic_eval.rs`

**Bash background task flow:**

1. Accept a command, validate permissions, and spawn a background process — `crates/aft/src/commands/bash_write.rs`, `crates/aft/src/bash_background/process.rs`
2. Buffer output, monitor timeout, and persist state to SQLite — `crates/aft/src/bash_background/buffer.rs`, `crates/aft/src/bash_background/watchdog.rs`, `crates/aft/src/bash_background/persistence.rs`
3. Poll status, drain completions, promote, or kill from command handlers — `crates/aft/src/commands/bash_status.rs`, `crates/aft/src/commands/bash_drain_completions.rs`, `crates/aft/src/commands/bash_promote.rs`, `crates/aft/src/commands/bash_kill.rs`

**Binary resolution flow:**

1. Check cache, npm platform package, PATH, and cargo install locations — `packages/aft-bridge/src/resolver.ts`
2. Download and checksum-verify a release asset when local resolution fails — `packages/aft-bridge/src/downloader.ts`
3. Start bridges against the resolved binary and hot-swap after version mismatch — `packages/aft-bridge/src/bridge.ts`, `packages/aft-bridge/src/pool.ts`

## Key Abstractions

**BinaryBridge:**
- Purpose: Keep one live `aft` subprocess available for request/response traffic.
- Location: `packages/aft-bridge/src/bridge.ts`
- Pattern: Persistent child-process adapter with timeout-triggered restart

**BridgePool:**
- Purpose: Scope bridges per OpenCode session and preserve isolated undo history.
- Location: `packages/aft-bridge/src/pool.ts`
- Pattern: Session-keyed object pool with LRU eviction

**Tool groups:**
- Purpose: Group related OpenCode tool definitions by capability surface.
- Location: `packages/opencode-plugin/src/tools/hoisted.ts`, `packages/opencode-plugin/src/tools/reading.ts`, `packages/opencode-plugin/src/tools/imports.ts`, `packages/opencode-plugin/src/tools/structure.ts`, `packages/opencode-plugin/src/tools/navigation.ts`, `packages/opencode-plugin/src/tools/refactoring.ts`, `packages/opencode-plugin/src/tools/safety.ts`, `packages/opencode-plugin/src/tools/conflicts.ts`, `packages/opencode-plugin/src/tools/lsp.ts`, `packages/opencode-plugin/src/tools/ast.ts`, `packages/opencode-plugin/src/tools/search.ts`, `packages/opencode-plugin/src/tools/semantic.ts`, `packages/opencode-plugin/src/tools/bash.ts`, `packages/opencode-plugin/src/tools/bash_write.ts`, `packages/opencode-plugin/src/tools/permissions.ts`, `packages/opencode-plugin/src/tools/_shared.ts`, `packages/opencode-plugin/src/tools/hoisted-internals.ts`
- Pattern: Thin TypeScript adapters over shared bridge transport

**AppContext:**
- Purpose: Centralize runtime state for commands inside the Rust worker.
- Location: `crates/aft/src/context.rs`
- Pattern: Interior-mutable service container for a single-threaded request loop

**VectorStore (trait):**
- Purpose: Decouple vector storage and similarity search from the semantic index lifecycle.
- Location: `crates/aft/src/vector_store.rs`
- Pattern: Trait with three built-in implementations — `FlatF32VectorStore` (f32 cosine similarity), `FlatBinaryHammingVectorStore` (packed binary Hamming search for quantized vectors), and `Model2VecVectorStore` (native f32 storage for model2vec backend).
- Used by: `crates/aft/src/semantic_index.rs`

**SemanticBackend (enum):**
- Purpose: Model the five embedding backends and their per-backend resolution rules (encoding, input mode, storage strategy, distance metric).
- Location: `crates/aft/src/config.rs`
- Variants: `Fastembed` (local ONNX), `OpenAiCompatible` (HTTP API), `Ollama` (local HTTP), `Perplexity` (contextualized document-chunk), `Model2Vec` (local static weights, gated by `semantic-model2vec` Cargo feature).
- Pattern: Enum-driven dispatch — each variant maps to default `OutputEncoding`, `InputMode`, `StorageStrategy`, and `DistanceMetric`.

**SemanticEmbeddingEngine (enum):**
- Purpose: Hold the live embedding model state for whichever backend is active.
- Location: `crates/aft/src/semantic_index.rs`
- Variants: `Fastembed`, `OpenAiCompatible`, `Ollama`, `Perplexity`, `Model2Vec` — each carries the client/model/configuration needed to produce embeddings.
- Pattern: Gated by `#[cfg(feature = "semantic-model2vec")]` for the Model2Vec variant; other variants are always compiled.

**Reranker:**
- Purpose: Send candidate search results to an LLM chat endpoint for relevance re-ordering.
- Location: `crates/aft/src/semantic_rerank.rs`
- Pattern: OpenAI-compatible chat completions call with a prompt template; falls back to original order on any error. Configurable via `rerank_enabled`, `rerank_model`, `rerank_base_url`, and `rerank_max_candidates` in `SemanticBackendConfig`.

**CallGraph:**
- Purpose: Cache per-file call data and answer callers, call-tree, impact, and trace queries.
- Location: `crates/aft/src/callgraph.rs`
- Pattern: Lazy workspace index with invalidation on watcher events

## Entry Points

**OpenCode plugin entry point:**
- Location: `packages/opencode-plugin/src/index.ts`
- Triggers: OpenCode loads the `@cortexkit/aft-opencode` plugin
- Responsibilities: Load config, resolve the binary, create the bridge pool, and register tool definitions

**Rust protocol entry point:**
- Location: `crates/aft/src/main.rs`
- Triggers: `packages/aft-bridge/src/bridge.ts` spawns the `aft` binary
- Responsibilities: Read NDJSON requests from stdin, dispatch handlers, drain watcher and LSP events, and write JSON responses

**Release automation entry point:**
- Location: `.github/workflows/release.yml`
- Triggers: Git tag pushes matching `v*`
- Responsibilities: Test the workspace, build platform binaries, publish crates and npm packages, and create a GitHub release

## Error Handling

**Strategy:** Return structured Rust `Response::error` payloads from command handlers, convert failed responses into plugin-side exceptions, and restart hung or crashed worker processes in `packages/aft-bridge/src/bridge.ts`.

## Honest Reporting Convention

**Goal:** an agent reading any AFT response must be able to distinguish three states without ambiguity: (1) the work could not be performed, (2) the work was performed and the result is complete, (3) the work was performed but the result is partial.

**Rule (tri-state):**

1. **`success: false` + `code` + `message`** — the requested work could not be performed. Codes are machine-actionable strings such as `"path_not_found"`, `"no_lsp_server"`, `"project_too_large"`, `"invalid_request"`, `"ambiguous_match"`. The agent must read the message before continuing.

2. **`success: true` + completion signaling** — the work was performed. Tools that produce results MUST report whether the result is complete and, if not, name the gaps. Conventional fields:
    - `complete: true` — the agent can trust absence of items in the result
    - `complete: false` + a named gap field — partial result. Gap fields include `pending_files`, `unchecked_files`, `scope_warnings`, `skipped_files: [{file, reason}]`, `walk_truncated`
    - `removed: bool` (mutations) — did the file actually change? `false` is a valid success when the requested change was a no-op.
    - `no_files_matched_scope: bool` (search tools) — distinguishes "the path/glob you gave me resolved to zero files" from "I searched N files and found nothing"

3. **Side-effect skip codes** — when the main work succeeded but a non-essential side step was skipped (e.g. post-write formatting), use a `<step>_skipped_reason` field so the agent gets specific feedback without treating the whole call as a failure. Approved values:
    - `format_skipped_reason`: `"unsupported_language"` | `"no_formatter_configured"` | `"formatter_not_installed"` | `"formatter_excluded_path"` | `"timeout"` | `"error"`
    - `validate_skipped_reason`: `"unsupported_language"` | `"no_checker_configured"` | `"checker_not_installed"` | `"timeout"` | `"error"`

**Anti-patterns this convention exists to prevent:**

- Returning `success: true` with empty results when the scope (path/glob) didn't resolve to any files — the agent reads it as "all clear" but really nothing was checked. Return `no_files_matched_scope: true` (when the scope was syntactically valid but matched zero files) or `success: false, code: "path_not_found"` (when a passed path doesn't exist).
- Reusing one skip-reason string for two distinct causes (e.g., `"not_found"` for both "language has no formatter configured" and "configured formatter binary missing"). The agent has different remediations for each — split them.
- Silently dropping files that fail to parse / open / decode inside a multi-file or directory operation. Always include a `skipped_files: [{file, reason}]` array so the agent knows X out of Y files were actually processed.
- Asserting `success: true` after a partial transaction without a `complete: false` flag and a list of pending work.

**Where this is documented in code:** `crates/aft/src/protocol.rs` `Response` doc comment carries the canonical rule and the approved field set. New tools must follow this convention; existing tools are migrating.

## Bash Output Compression

**Goal:** reduce hoisted-bash output to fewer tokens while keeping the information the agent actually needs (errors, summaries, ref updates) and discarding the noise (progress bars, repeated headers, deep nested directory listings).

**Four-tier dispatch in `crates/aft/src/compress/mod.rs`:**

1. **Specific Rust [`Compressor`] modules** — hand-written parsers for specific tools identified by tool token. Wins before broad package-manager modules. Each module lives in its own file under `crates/aft/src/compress/` and implements the `Compressor` trait (`fn matches(&str) -> bool` + `fn compress(&str, &str) -> String`). Current modules: `git.rs`, `cargo.rs`, `eslint.rs`, `biome.rs`, `tsc.rs`, `pytest.rs`, `vitest.rs`, `playwright.rs`, `mypy.rs`, `prettier.rs`, `ruff.rs`, `go.rs`, `next.rs`.
2. **Package-manager [`Compressor`] modules** — broad head-token matchers (`npm.rs`, `pnpm.rs`, `bun.rs`) that compress unclaimed package-manager output.
3. **Declarative TOML filters** — strip + truncate + cap + shortcircuit rules for the long tail of CLI tools, loaded from three sources at startup with project > user > builtin priority by filename:
    - **Builtin**: 22 filters shipped via `include_str!()` from `crates/aft/src/compress/builtin_filters/*.toml`, registered in `crates/aft/src/compress/builtin_filters.rs::ALL`
    - **User**: `<storage_dir>/filters/*.toml` (XDG-aware via the active `storage_dir`)
    - **Project**: `<project_root>/.aft/filters/*.toml` — gated by [`crate::compress::trust`]; never loaded for an untrusted project
4. **Generic fallback** — ANSI strip + consecutive-line dedup + middle-truncate. Always applies when no Rust module or TOML filter matches.

**Pipeline for TOML filters** (in `crates/aft/src/compress/toml_filter.rs::apply_filter`):

1. ANSI strip (when `[ansi].strip` is true; default true)
2. `[strip]` regexes drop matching lines (multiline mode)
3. `[shortcircuit]` checks remaining content; if matched, return `replacement`
4. `[truncate]` middle-truncates per line at `line_max` chars
5. `[cap]` enforces `max_lines` with `keep = "head" | "tail" | "middle"`

**Trust model** (`crates/aft/src/compress/trust.rs`): project filters can lie about output (e.g. strip real failures and replace with `tests: ok`). They are off by default. Users opt in via `npx @cortexkit/aft doctor filters trust`, which records the canonicalized project root in `<storage_dir>/trusted-filter-projects.json` (atomic temp-file rename, deserialized fail-closed). The CLI also exposes `untrust`, `trust --list`, `--show <name>`, and the default list view.

**Concurrency:** the filter registry is exposed as `Arc<RwLock<FilterRegistry>>` so the `BgTaskRegistry` watchdog thread can compress completed task output without holding `AppContext`. The compressor is installed as a closure on `BgTaskRegistry` from `crates/aft/src/main.rs` after `AppContext::new` constructs both.

**Configure invalidation:** `crates/aft/src/commands/configure.rs::handle_configure` calls `ctx.sync_bash_compress_flag()` and `ctx.reset_filter_registry()` on every configure so changes to `experimental.bash.compress`, `storage_dir`, `project_root`, or trust state pick up immediately without restart.

**Compression site:** terminal-state output only. Live tail of running tasks (via `bash_status` polling) is shown raw so agents debugging long commands see exactly what the process emitted. Compression fires inside `BgTaskRegistry::maybe_compress_snapshot` (status / list paths) and `enqueue_completion_locked` (completion frame + `bash_drain_completions` cache).

## Cross-Cutting Concerns

**Logging:** Write plugin logs through `packages/opencode-plugin/src/logger.ts` and Rust logs through `env_logger` in `crates/aft/src/main.rs`.

**Caching:** Cache resolved binaries in `~/.cache/aft/bin` through `packages/aft-bridge/src/downloader.ts`, cache session bridges in `packages/aft-bridge/src/pool.ts`, cache tool availability in `crates/aft/src/format.rs`, and cache call-graph state in `crates/aft/src/callgraph.rs`.

**Storage:** Store undo snapshots in `crates/aft/src/backup.rs`, named checkpoints in `crates/aft/src/checkpoint.rs`, persistent SQLite state in `crates/aft/src/db/` (bash task records, backups, compression events), background task output in `crates/aft/src/bash_background/persistence.rs`, pending UI metadata in `packages/opencode-plugin/src/metadata-store.ts`, and downloaded binaries in the cache directory managed by `packages/aft-bridge/src/downloader.ts`.
