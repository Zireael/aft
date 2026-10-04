# Engram's bound owner and unreadable callgraph view

## Read-only evidence (2026-10-04)

`ck module logs aft --since 48h -n 1000000` confirmed that this is not an
unbound-owner problem. For `/Users/ufukaltinok/Work/Projects/CortexKit/engram`:

- `2026-10-04T14:10:37.429Z`: `route 62@1 bound to root` that checkout.
- `2026-10-04T17:36:31.198Z` / `.200Z`: routes `65@1` and `75@1` bound there;
  `.306Z` / `.309Z`: routes `86@1` and `87@1` bound there too.
- Owner session `ses_ef906bd61ffe6EGopQHYlz75Ny`, at
  `2026-10-04T17:36:42.982Z`: `content-addressed view HEAD reuse 297/310`.
- The same session, at `2026-10-04T17:36:44.113Z`:
  `callgraph store configure warm failed: callgraph store unavailable: database is missing, stale, or mid-build`.
  The same failure occurred at `2026-10-04T14:10:53.658Z`.

There were no `plane=callgraph` events for this root in the captured window,
but there **were** `plane=views` publications, including
`2026-10-04T13:10:38.892Z`, `outcome=published candidates=310 blob_puts=2 pending_paths=0`.
Absence of legacy-plane build events therefore does not mean no graph was ever
materialized.

The family is `f5a961404e3622fd`, and the checkout scope is
`e7c3d96c39df7bf6`. The family's `artifact-owners/.../owner.json` names the main
checkout (PID 11248), not a linked worktree. Its legacy callgraph directory even
contains a `.current` pointer to `f5a961404e3622fd.g1790773222177850000.78670.sqlite`.
The selected view is
`14-1409-1791119431192558000-124-df36562751d3d697337e479f77e55c7a6ad62fcd5e6688ab71623fe8902083ba`.
Its manifest has non-null callgraph keys. Only a filesystem copy of the immutable
derived database and its SQLite sidecars was opened for inspection, never live
SQLite. The copy has 7,055 nodes, 290 bindings, `ready=1`, schema version `1`,
materialization version `7`, and fingerprint
`9e904a6dac7809c1250e41cc0eb9c1811afc33f707da25d3938c095b6e3bca2c`.
That fingerprint fails the current reader's build-output readiness check; the
regression fixture uses exactly that stored value.

## Mechanism

Source locations below refer to base commit `588185b897f3fba259396a67ce8f98713df6a50c`:

1. `context.rs:7097-7122` selects a pinned, HEAD-matching view and returns its
   reader error immediately. It never reaches writer promotion, legacy cold
   admission, or a breaker at `7229-7285` / `7502-7549`.
2. `callgraph_store/mod.rs:6781-6782` opens that derived database and calls
   `ensure_database_ready`. At `9718-9746`, readiness requires **both** `ready=1`
   and the current schema/build-output fingerprint. The current output version
   at `9770` is `v13-typed-dispatch-precision`; `ready=1` alone is insufficient.
3. `configure.rs:7167-7192` originally scheduled nothing when HEAD and membership
   matched and the manifest already contained callgraph keys. It did not check
   whether the derived graph was readable by this binary. Several bound sessions
   therefore repeated the error without scheduling a replacement.
4. Scheduling alone would not repair it: `views/assembly.rs:485-501` originally
   treated an identical manifest as a no-op, or reused a callgraph-equivalent
   derived database. Its incremental path could also copy incompatible output.

Thus the root cause is stale views output accepted as reusable by publication,
but rejected by query readiness. It is not the equivalent-rebind skip, owner
permission, a breaker, or a never-retried legacy limiter deferral. The warm error
was logged, but subsequent binds had no repair path.

## Repair and regression

Configure now schedules all HEAD members when the selected derived database
fails the same readiness check as its reader. Assembly consults that check before
no-op, derived reuse, or incremental cloning, and builds incompatible output cold
from the current immutable blobs. Changed extraction producer keys are rebuilt
by the owner, so read-only linked worktrees can publish and read their own views
from the shared family. Published files are never repaired in place.

The configure regression seeds an old-producer, same-HEAD view, binds the main
owner through several sessions, and checks real view readers for the owner and
two linked worktrees. A ready rebind must not add a generation. Separate assembly
regressions cover identical manifests and changed manifests so neither no-op nor
incremental reuse can preserve stale output. These checks run without embedding
servers, synthetic load, or changes to live daemon/storage state.

## Relation to the Pi standalone report (#399)

The reported `semantic artifact load proceeding without callgraph build_started
after 30s` is not enough to identify the same cause. An unreadable pinned view
does bypass legacy `plane=callgraph` starts, but returns `Error`, not an in-flight
build. `should_wait_for_callgraph_start` (`configure.rs:6180-6183` at the base)
requires `Building` **and** a receiver. Once maintenance reaches SemanticRelease
(`7062-7075`), Engram's stale-reader error should therefore release semantic
loading immediately, without waiting for that event. The timeout at `401-426`
can also mean maintenance has not reached that release step. No #399 view
metadata was inspected, so shared causality is unconfirmed; its request-loop
and restart paths are deliberately unchanged here.
