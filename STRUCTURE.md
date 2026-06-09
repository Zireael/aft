# Codebase Structure

## Directory Layout

```text
opencode-aft/
├── crates/                    # Rust workspace packages
│   ├── aft/                   # Core AFT library, CLI binary, command handlers, and integration tests
│   │   └── src/
│   │       ├── bash_background/  # Background task lifecycle, PTY, watchdog, persistence
│   │       ├── bash_permissions/ # Shell command permission analysis
│   │       ├── bash_rewrite/     # Shell command rewriting for Windows compatibility
│   │       ├── commands/         # One handler per protocol command
│   │       ├── compress/         # Shell output compression (Rust modules + TOML filters)
│   │       ├── db/               # SQLite persistent storage layer
│   │       ├── lsp/              # LSP client, transport, diagnostics
│   │       └── migrate_storage/  # Storage migration utilities
│   └── aft-tokenizer/         # Claude lookup-encoding tokenizer for code estimation
├── packages/                  # JavaScript workspace packages
│   ├── aft-bridge/            # Shared NDJSON bridge transport, binary resolution, ONNX runtime helpers
│   ├── aft-cli/               # Unified CLI — setup/doctor across all harnesses (@cortexkit/aft)
│   ├── opencode-plugin/       # OpenCode adapter that exposes and hoists AFT tools (@cortexkit/aft-opencode)
│   │   └── src/
│   │       ├── tools/         # One file per tool group
│   │       ├── hooks/         # Auto-update checker lifecycle
│   │       ├── shared/        # Shared utilities (RPC, PTY cache, status, session directory)
│   │       └── tui/           # TUI type definitions
│   ├── pi-plugin/             # Pi coding agent adapter for AFT (@cortexkit/aft-pi)
│   │   └── src/
│   │       ├── tools/         # One file per tool group
│   │       ├── commands/      # Pi slash commands (e.g., /aft-status)
│   │       ├── dialogs/       # Pi status dialog rendering
│   │       └── shared/        # Shared utilities (file discovery, PTY cache, status)
│   └── npm/                   # Platform-specific npm binary packages
├── benchmarks/                # Bun-based benchmark runner and reporting code
│   ├── aft-search/            # Semantic search benchmarks (corpus + results)
│   └── compression-tokens/    # Bash compression token savings benchmarks
├── scripts/                   # Release and version-management scripts
├── assets/                    # Repository assets such as the banner image
├── tests/                     # Docker-based integration tests, macOS/Windows E2E
│   └── pi-rpc/                # Pi RPC integration tests
├── docs/                      # Architecture and structure documentation
├── .github/workflows/         # Release automation workflows
├── Cargo.toml                 # Rust workspace manifest
├── package.json               # JavaScript workspace manifest
└── README.md                  # User-facing product and tool reference
```

## Directory Purposes

**`crates/aft-tokenizer/`:**
- Purpose: Provide Claude-compatible token counting for code estimation and context management.
- Contains: `src/` Rust sources, lookup-table encoding data generated at build time
- Key files: `crates/aft-tokenizer/src/claude.rs`, `crates/aft-tokenizer/build.rs`

**`crates/aft/`:**
- Purpose: Keep the Rust execution engine, stdin/stdout protocol binary, and shared analysis logic together.
- Contains: `src/` Rust modules, `tests/` integration suites, crate manifest
- Key files: `crates/aft/src/main.rs`, `crates/aft/src/lib.rs`, `crates/aft/src/commands/`, `crates/aft/tests/integration/`

**`crates/aft/src/commands/`:**
- Purpose: Add one handler file per protocol command.
- Contains: Command-specific request parsing and response generation
- Key files: `crates/aft/src/commands/read.rs`, `crates/aft/src/commands/write.rs`, `crates/aft/src/commands/outline.rs`, `crates/aft/src/commands/conflicts.rs`, `crates/aft/src/commands/semantic_search.rs`, `crates/aft/src/commands/semantic_doctor.rs`, `crates/aft/src/commands/semantic_eval.rs`, `crates/aft/src/commands/bash.rs`, `crates/aft/src/commands/bash_status.rs`, `crates/aft/src/commands/configure.rs`

**`crates/aft/src/bash_background/`:**
- Purpose: Manage background shell task lifecycle: spawn, buffer, watchdog, persist, and terminate.
- Contains: Process manager, PTY terminal emulation, output buffering, SQLite persistence, watchdog timeout monitoring, task registry
- Key files: `crates/aft/src/bash_background/mod.rs`, `crates/aft/src/bash_background/registry.rs`, `crates/aft/src/bash_background/process.rs`, `crates/aft/src/bash_background/pty_process.rs`, `crates/aft/src/bash_background/watchdog.rs`, `crates/aft/src/bash_background/persistence.rs`

**`crates/aft/src/db/`:**
- Purpose: Provide SQLite-backed persistent storage for backups, bash task records, and compression events.
- Contains: Schema management, CRUD operations, migration helpers
- Key files: `crates/aft/src/db/mod.rs`, `crates/aft/src/db/state.rs`, `crates/aft/src/db/backups.rs`, `crates/aft/src/db/bash_tasks.rs`, `crates/aft/src/db/compression_events.rs`

**`crates/aft/src/migrate_storage/`:**
- Purpose: Migrate persistent storage between versions (e.g., JSON → SQLite).
- Contains: Migration logic and logging
- Key files: `crates/aft/src/migrate_storage/log.rs`

**`crates/aft/src/compress/`:**
- Purpose: Compress shell command output to reduce token usage while preserving actionable information.
- Contains: Rust compressor modules (git, cargo, eslint, biome, tsc, pytest, etc.), TOML filter engine, trust model, builtin filter definitions
- Key files: `crates/aft/src/compress/mod.rs`, `crates/aft/src/compress/toml_filter.rs`, `crates/aft/src/compress/trust.rs`, `crates/aft/src/compress/builtin_filters/`

**`packages/opencode-plugin/src/hooks/`:**
- Purpose: Manage plugin lifecycle hooks, primarily the auto-update checker.
- Contains: Auto-update checker logic, version caching, hook activation
- Key files: `packages/opencode-plugin/src/hooks/auto-update-checker/index.ts`, `packages/opencode-plugin/src/hooks/auto-update-checker/checker.ts`, `packages/opencode-plugin/src/hooks/auto-update-checker/cache.ts`

**`packages/opencode-plugin/src/shared/`:**
- Purpose: Hold shared utilities consumed across the OpenCode plugin.
- Contains: RPC client/server, PTY cache, live server client, session directory helpers, TUI config, subagent detection, bash hints, status helpers
- Key files: `packages/opencode-plugin/src/shared/rpc-client.ts`, `packages/opencode-plugin/src/shared/rpc-server.ts`, `packages/opencode-plugin/src/shared/status.ts`, `packages/opencode-plugin/src/shared/pty-cache.ts`

**`packages/opencode-plugin/src/tui/`:**
- Purpose: Define TypeScript interfaces for the OpenCode plugin TUI surface.
- Contains: TUI type declarations
- Key files: `packages/opencode-plugin/src/tui/types/opencode-plugin-tui.d.ts`

**`packages/pi-plugin/src/commands/`:**
- Purpose: Provide Pi slash commands (e.g., `/aft-status`) for runtime diagnostics inside the Pi harness.
- Contains: Status command handler
- Key files: `packages/pi-plugin/src/commands/aft-status.ts`

**`packages/pi-plugin/src/dialogs/`:**
- Purpose: Render Pi dialog UIs for status and configuration views.
- Contains: Status dialog rendering
- Key files: `packages/pi-plugin/src/dialogs/status-dialog.ts`

**`packages/pi-plugin/src/shared/`:**
- Purpose: Hold shared utilities consumed across the Pi plugin.
- Contains: File discovery helpers, PTY cache, status helpers
- Key files: `packages/pi-plugin/src/shared/discover-files.ts`, `packages/pi-plugin/src/shared/status.ts`

**`benchmarks/aft-search/`:**
- Purpose: Run and report semantic search benchmarks.
- Contains: Benchmark corpus files and result data
- Key files: `benchmarks/aft-search/corpus/`, `benchmarks/aft-search/results/`

**`benchmarks/compression-tokens/`:**
- Purpose: Measure token savings from bash output compression across different CLI tools.
- Contains: Benchmark data, fixtures (build-test, deploy-container, filesystem, git, lint)
- Key files: `benchmarks/compression-tokens/data/`, `benchmarks/compression-tokens/fixtures/`

**`tests/pi-rpc/`:**
- Purpose: Test Pi-specific RPC integration flows between the plugin and the aft binary.
- Contains: RPC test helpers and fixture data
- Key files: `tests/pi-rpc/helpers/`, `tests/pi-rpc/fixtures/`

**`crates/aft/src/lsp/`:**
- Purpose: Keep LSP client, transport, registry, and diagnostics state separate from command handlers.
- Contains: LSP lifecycle modules and supporting types
- Key files: `crates/aft/src/lsp/manager.rs`, `crates/aft/src/lsp/client.rs`, `crates/aft/src/lsp/diagnostics.rs`

**`packages/opencode-plugin/`:**
- Purpose: Ship the OpenCode-facing package that resolves the binary and registers tools.
- Contains: `src/` TypeScript sources, `dist/` build output, tests, package manifest
- Key files: `packages/opencode-plugin/src/index.ts`, `packages/opencode-plugin/src/config.ts`, `packages/opencode-plugin/package.json`

**`packages/opencode-plugin/src/tools/`:**
- Purpose: Group OpenCode tool definitions by capability area.
- Contains: Thin adapters for hoisted, reading, import, structure, navigation, refactor, safety, AST, LSP, semantic, search, bash, conflict, and permission tools
- Key files: `packages/opencode-plugin/src/tools/hoisted.ts`, `packages/opencode-plugin/src/tools/bash.ts`, `packages/opencode-plugin/src/tools/reading.ts`, `packages/opencode-plugin/src/tools/refactoring.ts`, `packages/opencode-plugin/src/tools/semantic.ts`, `packages/opencode-plugin/src/tools/search.ts`, `packages/opencode-plugin/src/tools/_shared.ts`, `packages/opencode-plugin/src/tools/hoisted-internals.ts`, `packages/opencode-plugin/src/tools/permissions.ts`

**`packages/pi-plugin/src/tools/`:**
- Purpose: Group Pi tool definitions by capability area, mirroring the opencode-plugin tool structure.
- Contains: Thin adapters for hoisted, reading, AST, bash, structure, navigation, import, refactor, safety, semantic, LSP, conflict, diff-format, and fs tools
- Key files: `packages/pi-plugin/src/tools/hoisted.ts`, `packages/pi-plugin/src/tools/reading.ts`, `packages/pi-plugin/src/tools/bash.ts`, `packages/pi-plugin/src/tools/semantic.ts`, `packages/pi-plugin/src/tools/_shared.ts`, `packages/pi-plugin/src/tools/render-helpers.ts`, `packages/pi-plugin/src/tools/diff-format.ts`, `packages/pi-plugin/src/tools/fs.ts`

**`packages/opencode-plugin/src/__tests__/`:**
- Purpose: Verify plugin behavior, resolver logic, tool registration, and end-to-end bridge flows.
- Contains: Unit tests and `e2e/` test fixtures
- Key files: `packages/opencode-plugin/src/__tests__/tools.test.ts`, `packages/opencode-plugin/src/__tests__/e2e/`

**`packages/aft-bridge/`:**
- Purpose: Share NDJSON bridge transport, binary resolution, ONNX runtime helpers, and URL fetch across all harness adapters.
- Contains: `src/` TypeScript sources, tests, package manifest
- Key files: `packages/aft-bridge/src/bridge.ts`, `packages/aft-bridge/src/pool.ts`, `packages/aft-bridge/src/downloader.ts`, `packages/aft-bridge/src/resolver.ts`, `packages/aft-bridge/src/onnx-runtime.ts`, `packages/aft-bridge/src/url-fetch.ts`, `packages/aft-bridge/src/paths.ts`
- Used by: `packages/opencode-plugin/` and `packages/pi-plugin/` (both import from `@cortexkit/aft-bridge`)

**`packages/aft-cli/`:**
- Purpose: Provide the unified `npx @cortexkit/aft` CLI for setup, doctor, and filter management across all harnesses.
- Contains: `src/` TypeScript sources with harness-specific adapters and commands
- Key files: `packages/aft-cli/src/index.ts`, `packages/aft-cli/src/commands/doctor.ts`, `packages/aft-cli/src/commands/setup.ts`, `packages/aft-cli/src/adapters/opencode.ts`, `packages/aft-cli/src/adapters/pi.ts`

**`packages/opencode-plugin/`:**
- Purpose: Ship the OpenCode-facing adapter that resolves the binary, manages the bridge pool, and registers AFT tools with the harness.
- Contains: `src/` TypeScript sources, `dist/` build output, tests, package manifest
- Key files: `packages/opencode-plugin/src/index.ts`, `packages/opencode-plugin/src/config.ts`, `packages/opencode-plugin/package.json`

**`packages/pi-plugin/`:**
- Purpose: Ship the Pi coding agent adapter that registers AFT tools with the Pi harness.
- Contains: `src/` TypeScript sources, `dist/` build output, tests, package manifest
- Key files: `packages/pi-plugin/src/index.ts`, `packages/pi-plugin/src/config.ts`, `packages/pi-plugin/package.json`
- Same tool surface as opencode-plugin, adapted to Pi's plugin API

**`packages/npm/`:**
- Purpose: Publish one npm package per target platform so the plugin can resolve a bundled binary.
- Contains: Per-platform package manifests and `bin/` payload directories
- Key files: `packages/npm/darwin-arm64/package.json`, `packages/npm/linux-x64/package.json`, `packages/npm/win32-x64/package.json`

**`benchmarks/`:**
- Purpose: Run benchmark scenarios and post-process benchmark output with Bun.
- Contains: Benchmark source files, configs, cached results, package manifest
- Key files: `benchmarks/src/runner.ts`, `benchmarks/src/analyze.ts`, `benchmarks/package.json`

**`scripts/`:**
- Purpose: Automate release, validation, and version synchronization tasks.
- Contains: Shell and Node scripts
- Key files: `scripts/release.sh`, `scripts/version-sync.mjs`, `scripts/validate-packages.mjs`

## Key File Locations

**Entry Points:** `packages/opencode-plugin/src/index.ts`: Register OpenCode plugin tools and bridge configuration; `packages/pi-plugin/src/index.ts`: Register Pi plugin tools; `packages/aft-cli/src/index.ts`: Dispatch CLI commands (`setup`, `doctor`); `crates/aft/src/main.rs`: Start the Rust request loop; `.github/workflows/release.yml`: Drive tagged release publishing.

**Configuration:** `package.json`: Define Bun workspace scripts; `Cargo.toml`: Define the Rust workspace; `packages/opencode-plugin/src/config.ts`: Parse user and project AFT config.

**Core Logic:** `crates/aft/src/parser.rs`: Extract symbols and languages; `crates/aft/src/callgraph.rs`: Build navigation indexes; `crates/aft/src/edit.rs`: Run shared edit and diff logic; `crates/aft/src/semantic_index.rs`: Embed and search code by meaning across multiple backends; `crates/aft/src/vector_store.rs`: Vector storage abstraction; `crates/aft/src/semantic_rerank.rs`: LLM-based result reranking; `crates/aft/src/semantic_diagnostics.rs`: Search quality telemetry with WarningDedup; `crates/aft/src/semantic_doctor.rs`: Semantic health reports; `crates/aft/src/semantic_eval.rs`: Local retrieval evaluation via live search pipeline; `crates/aft/src/query_shape.rs`: Query classification for hybrid routing; `crates/aft/src/db/`: SQLite persistent storage; `crates/aft/src/bash_background/`: Background task lifecycle; `packages/aft-bridge/src/bridge.ts`: Manage subprocess transport.

**Tests:** `packages/opencode-plugin/src/__tests__/`: Plugin unit and e2e tests; `crates/aft/tests/integration/`: Rust integration tests.

## Naming Conventions

**Files:** Use capability-oriented filenames. Put Rust command handlers in snake_case files such as `crates/aft/src/commands/move_symbol.rs`. Put TypeScript tool groups in concise nouns such as `packages/opencode-plugin/src/tools/navigation.ts`. Use `.test.ts` for plugin tests and `_test.rs` for Rust tests.

**Directories:** Use lower-case descriptive directories. Group related runtime code under `packages/opencode-plugin/src/tools/`, `crates/aft/src/commands/`, and `crates/aft/src/lsp/`.

## Where to Add New Code

**New hoisted OpenCode file tool:** `packages/opencode-plugin/src/tools/hoisted.ts` — register the tool and map it onto a Rust command.

**New plugin tool group (OpenCode):** `packages/opencode-plugin/src/tools/[capability].ts` — export a `Record<string, ToolDefinition>` and wire it into `packages/opencode-plugin/src/index.ts`.

**New plugin tool group (Pi):** `packages/pi-plugin/src/tools/[capability].ts` — export a `Record<string, ToolDefinition>` and wire it into `packages/pi-plugin/src/index.ts`.

**New shared transport / binary-resolution code:** `packages/aft-bridge/src/[module].ts` — keep shared primitives (bridge, pool, downloader, resolver, ONNX, URL fetch) that both harness adapters consume.

**New OpenCode plugin shared utility:** `packages/opencode-plugin/src/shared/[module].ts` — add shared RPC, PTY cache, status, or session-directory helpers consumed across the plugin.

**New OpenCode plugin hook:** `packages/opencode-plugin/src/hooks/[hook-name]/` — add lifecycle hook logic and register it in the plugin bootstrap.

**New Pi slash command:** `packages/pi-plugin/src/commands/[command].ts` — add a Pi slash command handler and wire it into `packages/pi-plugin/src/index.ts`.

**New Pi dialog:** `packages/pi-plugin/src/dialogs/[dialog].ts` — add a Pi terminal dialog renderer and wire it into the Pi plugin.

**New Pi plugin shared utility:** `packages/pi-plugin/src/shared/[module].ts` — add shared helpers consumed across the Pi plugin.

**New unified CLI command:** `packages/aft-cli/src/commands/[command].ts` — add the handler and dispatch it from `packages/aft-cli/src/index.ts`.

**New Rust command handler:** `crates/aft/src/commands/[command_name].rs` — expose the handler from `crates/aft/src/commands/mod.rs` and dispatch it from `crates/aft/src/main.rs`.

**New shared Rust engine code:** `crates/aft/src/[domain].rs` — keep reusable parser, formatter, import, analysis, or semantic code outside command handlers.

**New semantic backend:** `crates/aft/src/semantic_index.rs` — add a new variant to `SemanticBackend` (config), `SemanticEmbeddingEngine` (engine), and implement `embed_texts` for the new backend. Add a new `VectorStore` implementation in `crates/aft/src/vector_store.rs` if the backend uses a different vector format. Update `crates/aft/src/config.rs` with default parameters.

**New query classification strategy:** `crates/aft/src/query_shape.rs` — add a new `QueryKind` variant and wire routing weights into the hybrid search pipeline.

**New semantic reranking strategy:** `crates/aft/src/semantic_rerank.rs` — implement a new reranking approach and wire it into `crates/aft/src/commands/semantic_search.rs`.

**New background bash task behavior:** `crates/aft/src/bash_background/[module].rs` — add the lifecycle module and register it in `crates/aft/src/bash_background/mod.rs`.

**New persistent storage table:** `crates/aft/src/db/[table].rs` — define the schema, add CRUD operations, and register in `crates/aft/src/db/mod.rs`.

**New LSP behavior:** `crates/aft/src/lsp/[module].rs` — keep transport and server-management code inside the LSP subsystem.

**New tokenizer or Claude encoding code:** `crates/aft-tokenizer/src/[module].rs` — keep the tokenizer crate focused on Claude-compatible lookup encoding.

**New platform binary package:** `packages/npm/[platform-key]/` — add `package.json` and ship the platform binary in `bin/`.

**New plugin tests (OpenCode):** `packages/opencode-plugin/src/__tests__/` or `packages/opencode-plugin/src/__tests__/e2e/` — follow the existing `*.test.ts` naming.

**New plugin tests (Pi):** `packages/pi-plugin/src/__tests__/` — follow the existing `*.test.ts` naming.

**New Rust integration tests:** `crates/aft/tests/integration/` — follow the existing `*_test.rs` naming.
