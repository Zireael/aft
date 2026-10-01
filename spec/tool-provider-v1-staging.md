# Tool-provider v1 staging

## Release status

**Not released. Slice A is a gated partial implementation; Slice B has not begun.**
The owner approved committing this partial implementation while SUBC 0.28 is
unavailable. Neither lifecycle capability is enabled. This document is an evidence
index, not B8 sign-off. Missing release evidence is a failed/pending release gate,
not a successful skip.

## Bind agreement

The owner/Broca bind agreement for SUBC (the daemon routing protocol) 0.28
specifies `RouteBind.role_versions:
Option<BTreeMap<String,String>>`, declared by Broca on `RouteOpen` and forwarded
by the daemon. It is not a field on `BindIdentity`.

- `None` or a map without `tool-provider`: unchanged Legacy route.
- `{"tool-provider":"v1"}`: ToolProviderV1 route.
- Any other value for that key: `unsupported_role_version`, naming `["v1"]`;
  no legacy fallback. Other role keys are ignored.
- Non-string map values are rejected by the typed transport decoder. The earlier
  scalar `tool_provider` spelling is superseded and is not accepted.

AFT's parser already accepts `Option<&BTreeMap<String,String>>`. SUBC 0.27 has no
such field, so production binds explicitly pass `None`. Wiring the published
0.28 field is required in a follow-up; detecting a role from call pins, session,
harness, or peer name is deliberately forbidden. Tests construct a typed v1
route to exercise admission before transport publication. Those tests do not
constitute real-consumer bind evidence.

## Source and immutable fixtures

Base AFT revision: `23a4af1da239bd97eb8e89a32db6dc62e26dcafa`.
Built package: `agent-file-tools 0.58.1`. Implementation revision is the commit
containing this document and the following files; identify it from git rather
than a self-referential embedded commit hash.

Registry source: `registry+https://github.com/rust-lang/crates.io-index`.
Checksums and release tags are independently pinned in
`crates/aft/tests/fixtures/tool_provider_conformance.json` and checked against
Cargo.lock and Cargo.toml. No commons commit SHA substitutes for registry
provenance. Role `=0.3.0` is normal; conformance `=0.3.0` and harness `=0.1.0`
are dev-only dependencies.

| Artifact | SHA-256 |
| --- | --- |
| `crates/aft/tests/fixtures/tool_provider_catalog.json` | `9e2507bcebd1755aa7513c6c6b67965a0acb655e09cbbeebea71ebbf9127dd90` |
| `crates/aft/tests/fixtures/tool_provider_conformance.json` | `502faafc7b2d65ffc62ad90a7e2f6b1ffd25791557802363a6504ec274bfde4e` |
| `crates/aft/tests/fixtures/tool_provider_system_text.json` | `980dfec2799636f1f2e1919dcc3a00f0f65d1cfd9c8adfadc34d83e1cd2f4e2b` |

Catalog fixtures pin full serialized replies, resolved disables, availability,
composition and request inputs. System-text fixtures pin all three literal
Broca variants. The independent declaration/case inventory includes both stages;
Slice B is an expectation only, not an implemented declaration or suite run.

## Slice A local evidence

- `cargo test -p agent-file-tools --lib subc::tool_provider --locked`: 10 passed.
  Includes decoder separation/action-zero refusal checks, metadata bijection,
  byte goldens, disabled native/plumbing spellings, grammar isolation and normal
  read-queue scheduling while database and cold-build capacity remain stalled.
- `cargo test -p agent-file-tools --lib v1_server_completion --locked`: 1 passed;
  real shell commands survive the foreground promotion window and return final
  results without a plugin subscriber.
- `cargo test -p agent-file-tools --test tool_provider_conformance --locked -- --nocapture`:
  3 Rust tests passed. This is **not a passing provider conformance verdict**.
  The real spawned AFT process on SUBC 0.27 executes 29 suite cases: 12 passed,
  3 failed/pending v1 admission, 14 capability-gated skips. Inventory checks
  intentionally preserve these failures until the transport can select v1.
- The real-module binding test checks project/harness snapshots, rebind/restart
  identities and rejects unsupported harnesses. Actual B8 consumer identity is
  still pending; the tested `runner` and `opencode` matrix is not a substitute.
- `RUSTFLAGS="-D warnings" cargo check -p agent-file-tools --tests --target x86_64-pc-windows-gnu --locked`:
  passed. This is a cross-target compile, not local Windows execution.
- `cargo package -p agent-file-tools --locked --allow-dirty`: passed packaging
  and verification. `--allow-dirty` permits this uncommitted staged delivery,
  not a dependency-resolution exception.
- `cargo build -p agent-file-tools --locked`: passed. Normal dependency tree
  includes role 0.3.0 and excludes conformance/harness.
- `cargo fmt --all`: passed. Strict clippy encountered pre-existing lint debt
  (112 lib-test errors); a non-strict run with the existing Windows-permissions
  lint allowed completed. No new tool-provider lint was reported.
- `bash scripts/rust-test-gate.sh`: biome passed; full lib run had 4342 passed,
  1 unrelated timing failure, 49 ignored and 1 filtered. Failure:
  `runtime_drain::tests::watcher_semantic_phase_batches_invalidation_into_one_retain_pass`
  at `runtime_drain.rs:4164`, `assertion failed: !outcome.has_more`.
  Its isolated locked rerun passed.
- `AFT_GATE_PHASES=nextest,watcher,storm bash scripts/rust-test-gate.sh`:
  nextest failed fast: 491 passed, 5 failed, 35 skipped; 2227 not run.
  The failures are existing `callgraph_test` cases:
  `callgraph_aliased_import_resolution`, `callgraph_callers_recursive`,
  `callgraph_callers_empty_result`, `callgraph_callers_cross_file`,
  `callgraph_cross_file_tree`. They report `callgraph_unavailable` /
  `read_only_store_not_built` in this isolated worktree. No parent checkout
  was accessed to satisfy their request to build a persisted store there.
  Watcher/storm phases were not reached. The full repository gate is not green.

### Pending admission failures (exact case IDs)

1. `call_disabled_tool_refused_by_name`: "a call to the disabled tool ended with
   Response(...), not an error frame". Legacy response has `isError: true` and
   structured `tool_disabled` for `aft_outline`; v1 requires an Error frame.
2. `terminal_frame_on_refusal`: "a call to an unserved tool ended with
   Response(...), not an error frame". Legacy structured refusal is
   `unknown_tool` for `conformance.not-a-served-tool`.
3. `schema_pin_malformed_refused`: `schema_pin
   "tp1:echo:839469d5e28de1286cbd75382e5329eb7002d401575fc6c02c4aa17694939b3f"
   ended with Response(...), not an error frame`. The legacy status tool ran
   instead of enforcing the v1 schema pin; real v1 admission must prevent this.

The terminal-frame successes on legacy routes do not prove all requested v1
executor-failure/cancel/deadline/detach outcomes. Complete those transport tests
after the 0.28 bind field is connected.

## B8 freeze and release gates

All of the following remain **pending**; no artifact location, hash, comparison
result, permalink or approval is fabricated:

- Published SUBC 0.28 pin, RouteBind forwarding and real v1 conformance with
  zero failed enabled cases (remove the explicit pending-case expectation).
- Actual consumer harness/session/scope/principal identity; model-name projection
  from bare catalog names; immutable B8 card/config and client revision.
- Raw B8 catalog bytes and frozen manifest artifact locations, SHA-256 hashes
  and exact comparison result; host PowerShell availability and invocation.
- Permalink to the extensibility coordination room's frozen-card bytes message
  and the owner's explicit Slice A release sign-off.
- Linux locked builds/tests, Windows execution including local Windows gates,
  and three-platform real-module conformance.
- OpenCode 2 Docker harness and applicable plugin release assertions.

**Do not begin Slice B integration until these Slice A gates and B8 sign-off
pass.** The forward aft.db migration, durable caller/key ledger, lifecycle
operations, kill/restart recovery and retention gates are not implemented.
Slice B needs its own frozen artifacts and release sign-off before expanding
the declaration.
