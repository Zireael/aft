# Semantic backend status: a released-port fixture race

## CI failure and comparison

`gh run view 37388045011 --log-failed` resolves to `cortexkit/aft`. The failed
Linux job checked out `02b213e1b7027e9ed4910a803c558610e9c8ee2d` (train 301).
Its library run reported 4,953 passed, one failed, 45 ignored, and one filtered
out. The only failure was
`commands::semantic_search::tests::status_names_an_unreachable_backend_before_the_build_reaches_it`:

```text
assertion `left == right` failed:
{"status":"loading","state":"loading","refreshing_count":0,"stage":"loading_artifacts","files":null,"entries_done":null,"entries_total":null,"backend":"openai_compatible","model":"all-MiniLM-L6-v2"}
  left: String("loading")
 right: "backend_unavailable"
```

The test, remote-backend probe, and status handler are byte-identical between
that train commit and the tested `origin/main` commit,
`02315234cb830ab9045efdd183e40cfefdf2284e`. Their Git blob IDs are:

| File | Blob |
| --- | --- |
| `crates/aft/src/commands/semantic_search/mod.rs` | `b59828f2032b16a258f517c840eb9e6218a8475a` |
| `crates/aft/src/semantic_index.rs` | `019ad6eb7a90bf9af94e483d9cdb86e84538dcfa` |
| `crates/aft/src/commands/status.rs` | `983fc92fabafe29cdef3254258d822e05fdd7e5e` |

The repetitions below ran on macOS with cargo/rustc 1.99.0. Each revision used
a throwaway HOME and existing `mkdir -p`-created XDG config, cache, data, state,
and runtime directories. CARGO_HOME and RUSTUP_HOME remained the real ones;
AFT_STORAGE_DIR was not set. Tests within the module used the Rust harness's
default parallel execution, not serial execution.

```text
cargo test -p agent-file-tools --lib -- commands::semantic_search::tests::status_names_an_unreachable_backend_before_the_build_reaches_it
cargo test -p agent-file-tools --lib -- commands::semantic_search::tests::
```

Each command ran 20 times on each revision:

| Revision | Status test alone | Parallel semantic-search module |
| --- | --- | --- |
| `next301` (`02b213e1`) | 20/20 passed, one test each | 20/20 passed, 104 tests each |
| `origin/main` (`02315234`) | 20/20 passed, one test each | 19/20 passed, 104 tests each |

The status test passed in all 80 runs. Main's nineteenth module run failed four
unrelated elapsed-time assertions: `blackholed_backend_never_blocks_status_or_search`,
`blocked_local_query_times_out_falls_back_and_late_result_populates_cache`,
`building_indexes_refuse_with_each_lanes_status_instead_of_walking`, and
`contended_search_index_degrades_with_disclosure`. Those assertions are not
changed here. These repetitions do not reproduce the Linux failure on demand
and cannot establish which other test owned the CI port.

## Deterministic reproducer

The status test used to bind a temporary TCP listener to port zero, remember
the allocated port, and immediately drop the listener. It then treated that
released port as a permanently unreachable backend. A parallel test or another
process can bind it before the asynchronous probe connects.

Forcing this legal interleaving by binding a second listener to the released
address before the first status request makes the original test fail on the
train commit, alone, with precisely the CI response above. No injected sleep
or longer timeout is needed. A TCP probe reports that listener as reachable,
caches success for 30 seconds, and therefore correctly leaves the semantic
index loading. Polling for five seconds cannot turn a reachable endpoint into
an unreachable one. A probe that was merely still pending would instead show
the `checking_embedding_backend` stage, absent from the failure.

This exposes a pre-existing fixture race rather than a changed status contract.
The same released-port assumption is present in main's byte-identical code.
The CI log has no connection tracing, so port reclamation is a demonstrated
cause of the exact symptom, not an observation of the original CI connection.

## Fix and regression defense

The test-only `reserved_refused_backend_for_test` fixture binds a TCP socket
without calling listen and returns both the owning socket and its address.
Callers hold the socket until their test ends. Connections fail, but another
listener cannot take the address. The semantic-search disclosure and
query-embedding-failure tests and the direct refused-backend probe test now
share this fixture.
Production probing, cache TTLs, polling intervals, and deadlines are unchanged.

On macOS a connect to a bound non-listening socket takes the probe's existing
two-second connect timeout instead of immediately returning connection refused.
Both results correctly mean unavailable. An attempted migration of the separate
`refused_connection_is_an_immediate_honest_transient_failure` embedding test
failed its immediate-refusal error assertion, so that migration was reverted.
That test's original contract and fixture are unchanged; its released-port
assumption remains an adjacent risk, not a change in this fix.

`refused_backend_fixture_keeps_its_port_reserved` asserts that a competing
listener cannot bind the address; `remote_backend_probe_names_a_refused_connection`
still exercises the real TCP probe and its failure reason. The original status
test still asserts the status, URL, and TCP failure reason.

Two mutation controls release the fixture's socket while retaining its published
address. The reservation test must then fail. With a competing listener also
claiming that released address in the status test, the status assertion must
again fail with `loading`, while the other semantic-search tests remain green.
Mutation edits are restored from the staged live implementation before delivery.

Both controls did fail as expected:

```text
semantic_index::tests::refused_backend_fixture_keeps_its_port_reserved ... FAILED
a parallel listener must not be able to claim the refused backend's port
test result: FAILED. 0 passed; 1 failed

commands::semantic_search::tests::status_names_an_unreachable_backend_before_the_build_reaches_it ... FAILED
  left: String("loading")
 right: "backend_unavailable"
test result: FAILED. 103 passed; 1 failed
```

The two `remote_backend_probe_*` controls passed under the mutation. The
unavailable-backend search disclosure and query-embedding-failure fallback tests
also passed in the mutated module run; only the status test with the injected
competing listener failed. The unstaged diff during mutation was two Rust files,
five insertions and one deletion; after staged-file restoration it was empty.

The changed Rust files fall under the search-quality ranking fence, but the
changes are confined to `cfg(test)` fixtures and tests. The train's engine-unwired
harness descriptor records that distinction; there is no ranking or routing
change to benchmark.
