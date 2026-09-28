# Inspect LSP startup and repository writes

## Cargo.lock reproduction and protection

Verified with the PATH-installed `rust-analyzer 1.98.1 (48a229ce 2026-09-01)`.
AFT discovers an external executable, rather than bundling a fixed analyzer version.
The regression `inspect_command_test::scoped_rust_inspect_preserves_stale_cargo_lock`
creates a dependency-free local/path-dependency workspace, generates its lockfile
with `cargo generate-lockfile --offline`, then changes the dependency manifest
version from 0.1.0 to 0.2.0. It invokes the real blocking Rust-scoped inspect handler
and compares the lockfile bytes, not a proxy or mocked Cargo command.

Removing both locked arguments recreates the old startup behavior and produces:

```text
test inspect_command_test::scoped_rust_inspect_preserves_stale_cargo_lock ... FAILED
assertion `left == right` failed: Rust-scoped inspect rewrote the stale Cargo.lock
test result: FAILED. 0 passed; 1 failed; 0 ignored
```

The rewritten lock entry has `local-dep` version 0.2.0 instead of 0.1.0.
With the arguments restored the same test passes, and inspect reports a
`failed_producer` gap for Rust carrying the Cargo `--locked` failure reason.

The installed analyzer's `--print-config-schema` documents:

- `cargo.extraArgs`: extra arguments to every Cargo invocation;
- `cargo.metadataExtraArgs`: extra arguments specifically for metadata;
- `check.extraArgs`: extra arguments for cargo check;
- `cargo.targetDir`: optional analyzer-specific output directory (default null).

See also the [upstream configuration reference](https://rust-analyzer.github.io/book/configuration.html#cargo.extraArgs).
AFT sets **both** Cargo argument lists to `--locked`. Metadata-specific arguments
matter: on the tested version, setting only `cargo.extraArgs` preserved the lock
but yielded a generic build-script failure instead of the actionable metadata
failure. Check/build-script discovery uses the general Cargo arguments; no
custom check/build-script command is supplied by AFT. Explicit user overrides
of initialization options or commands can change these guarantees.

Analyzer `experimental/serverStatus` warning/error messages were previously
ignored while quiescent reports were promoted to authoritative. AFT now retains
the failure, invalidates earlier reports, keeps subsequent reports provisional,
and reports the failed producer as settled-but-unavailable. A later healthy
quiescent status permits normal reporting again.

The integration test skips with a named stderr reason when `rust-analyzer
--version` is unavailable (including a rustup shim without its component).
The Linux nextest execution shards previously did not request the component;
they now install it explicitly. Other runners may still take the documented skip.

## Remaining filesystem effects and limits

This is lockfile protection, **not a filesystem sandbox**. No general read-only
process boundary exists around built-in or user-supplied LSP executables.

| Server / operation | Write behavior and AFT policy |
| --- | --- |
| rust-analyzer Cargo check/build-script discovery | AFT does not set `cargo.targetDir`, so Cargo uses the workspace/toolchain-configured target directory (normally `target/`). These are expected build artifacts, usually ignored, not a guarantee that a user's ignore rules cover them. Cargo registry/git caches can also be written outside the repository. |
| Rust build scripts and procedural macros | Arbitrary project/dependency code can write outside `target/`; `--locked` does not constrain it. A strict no-source-writes guarantee requires a sandbox or read-only source mount with writable artifact/cache directories, or refusing analysis that needs executable build hooks. Disabling build scripts/proc macros would reduce coverage and is not silently done here. |
| typescript-language-server / tsserver | AFT requests language-service diagnostics, not `tsc --build`, emit, or compile-on-save. Incremental/composite compiler settings alone do not turn diagnostic requests into `.tsbuildinfo` emission. See [TypeScript language-service design](https://github.com/microsoft/TypeScript/wiki/Using-the-Language-Service-API#design-goals), which separates diagnostics and emit. Plugins and automatic type-acquisition caches remain external code/effects; this is not a sandbox guarantee for arbitrary plugins. |
| Ruby LSP | AFT launches bare `ruby-lsp`. Its [documented composed bundle](https://shopify.github.io/ruby-lsp/composed-bundle.html) creates `.ruby-lsp/Gemfile`, runs bundle install, and may auto-update the server gems. Startup is not filesystem-read-only. |
| ElixirLS | Its [automatic build](https://github.com/elixir-lsp/elixir-ls#automatic-builds-and-error-reporting) uses `mix compile`; [Dialyzer](https://github.com/elixir-lsp/elixir-ls#dialyzer-integration) writes `.elixir_ls/dialyzer_manifest`. Project compiler tasks may have additional effects. |
| Other built-ins | Registry commands include build-system-aware servers (JDT LS, FSAutoComplete, Roslyn, Kotlin LS, SourceKit, Gleam, Haskell, ZLS), cache/index servers (clangd, clojure-lsp, Intelephense, Lua LS), and configurable tools (gopls, Tinymist, Texlab, Dart, Julia, Nix, OCaml, web/Python linters). AFT supplies no common filesystem-write fence. This audit does not certify each version/plugin/build system as write-free or claim that every server writes source files. |

Recommendation: treat automatic build/bundle/export-capable servers as requiring
an explicit write policy before claiming inspect is strictly read-only. Prefer
read-only source mounts or OS sandboxing with dedicated writable caches; where
that cannot be provided, refuse automatic startup and report the named producer
and reason, rather than silently executing project hooks. Audit server versions
and user overrides under that policy. Do not restore files after analysis as a
substitute: restoration can race user edits and cannot undo arbitrary hooks.
