# AFT concurrency limits

AFT's entry for the CortexKit fleet concurrency spec (section 3). Every value is read from code at the cited location; update this file when a limit changes.


Values below are production defaults. Paths are relative to `aft/`; a bound called *process-wide* is shared across roots/sessions. These are separate from the daemon/SDK limits in §1.

## Executor and builds

| Limit / key | Value and bound | Source |
|---|---|---|
| `ExecutorConfig::default().pool_size` | `clamp(available_parallelism - 1, 2, 8)` (fallback CPU count 2); general executor threads, effective range 2–8. | `crates/aft/src/executor/mod.rs:118-135,160-163` |
| `BIND_RESERVE_WORKERS` | 2 additional RouteBind-only threads, total 4–10; no general work uses them. Bind writer promotion after 500 ms (`BIND_PROMOTION_AGE`), other interactive writers after 6 s. | `crates/aft/src/executor/mod.rs:83-100,152-157` |
| `Lane`, `JobClass` | Five lanes: `PureRead`, `SerialLspStatus`, `HeavyInit`, `Mutating`, `MaintenanceCommit`; two classes: `Interactive`, `Maintenance`. Read/actor cap defaults `clamp(pool_size-1,1,4)`; heavy permits `clamp(pool_size-1,2,3)` (effective heavy cap 1–3, leaving a general worker). | `crates/aft/src/executor/mod.rs:32-65,118-133,160-174` |
| `interactive_reserve`, `maintenance_cap` | Reserve 2 general threads when pool ≥4, else 1; maintenance in flight ≤ `max(1, pool_size - reserve)` = 1–6. | `crates/aft/src/executor/mod.rs:175-188` |
| `MAINTENANCE_QUEUE_CAP`, `MaintenanceCoalesceKey` | 512 queued maintenance jobs **per actor**; coalesce queued WatcherDrain/LspDrain/StandingPass/ConfigReload by root and key; at cap remove duplicate drains then refuse if still full. | `crates/aft/src/executor/mod.rs:30,67-79,1452-1507` |
| `DEFAULT_COLD_BUILD_LIMIT`, `GLOBAL_COLD_BUILD_LIMITER` | 2 concurrent cold builds **process-wide** (test build 1024); waiting maintenance/inspect/standing classes share slots, root-aware requests may share a permit. | `crates/aft/src/cold_build_limiter.rs:6-17,31-45,77-90,110-116` |
| `WARM_SEARCH_RELOAD_LIMIT` | 4 concurrent warm search-index parses/verifications **process-wide**, isolated from cold builds. | `crates/aft/src/commands/configure.rs:4295-4302` |
| `BuildDeathBreaker` | Per root/domain/fingerprint: trip at `ZERO_CREDIT_DEATH_LIMIT=3`, `CREDITED_DEATH_LIMIT=6`, or `IN_BUILD_BURN_LIMIT_MS=30 min`; `TRIP_TTL_MS=24 h`; marker heartbeat 5 s/recent 15 s. | `crates/aft/src/build_breaker.rs:14-23,76-83` |

## Daemon edge and bridge

| Limit / key | Value and bound | Source |
|---|---|---|
| `ROUTE_BIND_DEADLINE` / `RouteBindAck` | 12 s pending bind deadline; successful configure enqueues ack before post-bind maintenance. Relay also expires at 12 s (§1). | `crates/aft/src/subc/mod.rs:153-154,5317-5330,5358-5403` |
| `MODULE_DRAINING_WINDOW_CAP` | `min(daemon-supplied remaining deadline, 120 s)` protects against bogus notices; normally 30 s. End bg_events with `StreamEnd`; detach held bash, settle permission asks/deferred responses, let normal tool calls/binds finish; log held-request census at start/quiescence/deadline/connection end. | `crates/aft/src/subc/drain.rs:1-70`; `crates/aft/src/subc/mod.rs:3862-3880,4292-4350,4692-4710` |
| `LSP_SHUTDOWN_ALL_BUDGET` / graceful index flush | 1.5 s total LSP shutdown; natural stdin EOF waits at most 300 ms for search-index flush thread; callgraph refresh flush follows synchronously. | `crates/aft/src/lsp/manager.rs:37-39,2461-2474`; `crates/aft/src/main.rs:602-623` |
| `ready: false`, `PLAIN_START_WARM_BUDGET`, `SWAP_CANDIDATE_WARM_BUDGET` | Manifest initially unready; 10 s normal start, 90 s swap candidate (incumbent serves). Live-roots query ≤5 s, `catalog.update` flip attempt ≤5 s with 250 ms–10 s backoff. | `crates/aft/src/subc/manifest.rs:356`; `crates/aft/src/subc/readiness.rs:45-69` |
| `ROUTE_OPEN_RELOAD_WAIT_CEILING_MS` | 45 s ceiling on waiting out reload refusals; actual wait budget is min(call's `timeoutMs` or 30 s SDK default, 45 s); retry delay 100 ms–2 s. The whole call (route opens, retry sleeps, the request) is measured from its start on the monotonic clock: each `routeOpen` is raced against the time left (subc-client's `routeOpen` takes no deadline), `client.request` gets only the time left, and past the deadline the call surfaces the last refusal (or a `request_deadline` error) with the "did not return within this call's Ns deadline" suffix. | `packages/aft-bridge/src/subc-transport.ts:224-266,802-835,1711,1764-1832,1915-1971` |
| `MAX_CONSECUTIVE_TRANSPORT_FAILURES`, `LIVENESS_PROBE_TIMEOUT_MS` | After 3 unanswered transport calls probe the same pooled connection for 15 s before dropping it; successful request resets counter. | `packages/aft-bridge/src/subc-transport.ts:205-219,1793-1803,1823-1835` |
| Bridge transport deadline | `DEFAULT_BRIDGE_TIMEOUT_MS=30 s`; per-command overrides 60 s for callers/callgraph/trace*/impact/inspect/grep/glob/search/semantic_search; passive status ≤5 s, with no hang escalation. | `packages/aft-bridge/src/bridge.ts:18-20,872-887`; `packages/aft-bridge/src/command-timeouts.ts:15-27,34-56` |

## Tool calls, bash, storage, relay, LSP

| Limit / key | Value and bound | Source |
|---|---|---|
| `inspect.diagnostics_timeout_ms` | Default 120 s, clamped 10–600 s; **whole request** shares one deadline, stops work 5 s before terminal; each phase waits ≤ min(half remaining work budget, 60 s). Plugin transport budget adds 30 s. `inspect.tier2_pass_timeout_ms` defaults 600 s, separately configurable without a clamp in resolver (pass budget, not request deadline). | `crates/aft/src/config.rs:21-24,296-314`; `crates/aft/src/config_resolve.rs:1972-1988`; `crates/aft/src/commands/inspect.rs:28-36,50-73`; `packages/opencode-plugin/src/tools/inspect.ts:13,347-350` |
| `FIRST_SEARCH_INDEX_LOAD_WAIT_BUDGET` | 2.5 s first `aft_search` wait for a concurrently loading index. | `crates/aft/src/commands/semantic_search/mod.rs:145-162` |
| `AFT_CALLGRAPH_BUILD_WAIT_MS` | 0 ms default (async `Building`), optional environment milliseconds; inline wait for a cold callgraph build; no production clamp found. | `crates/aft/src/context.rs:3202-3212` |
| `DEFAULT_BG_TIMEOUT`, `foreground_wait_window_ms` | 30 min default command runtime, configurable per bash command by `timeout`; 15 s default foreground auto-promotion window (internal setting); `wait:true` can wait until command exits. | `crates/aft/src/bash_background/registry.rs:68,1894`; `crates/aft/src/config.rs:589-591,750-752`; `crates/aft/src/commands/bash_orchestrate.rs:14,489` |
| `bash.watch_sync_max_ms`, `MAX_WATCHES_PER_TASK` | Plugin sync-watch max default 120 s, clamped 1–1800 s; a watch defaults to 30 s in primary. Up to 8 watches per task. | `crates/aft/src/config.rs:25-27,504-507`; `packages/opencode-plugin/src/tools/bash_watch.ts:74,138`; `crates/aft/src/bash_background/watches.rs:5,91-92` |
| Bash output and watchdog | Final 16 KiB (6 KiB head/10 KiB tail); running preview 8 KiB; raw/structured reply 50 KiB; compression input 10 MiB (4+6 MiB); watchdog tick 500 ms. Foreground orchestration polls every 100 ms **off executor workers** between short read jobs. | `crates/aft/src/bash_background/output.rs:3-34`; `crates/aft/src/bash_background/watchdog.rs:8-14`; `crates/aft/src/subc/mod.rs:160-163` |
| `STEADY_BUSY_TIMEOUT`, deferred `open` | SQLite aft.db busy timeout 5 s; deferred open/migration retries for 10 s, backoff 10–200 ms, per-attempt initialization busy wait ≤100 ms; single-attempt request path uses `TOOL_RETRY_BUSY_WAIT=250 ms` and does not repeat deferred wait. | `crates/aft/src/db/mod.rs:423-427,489-539` |
| `FOLD_BUSY_WAIT`, maintenance cadence | Ledger fold SQLite wait 250 ms and try-lock of process DB mutex; failed folds remain pending for next run, at most once/minute in production. | `crates/aft/src/db/write_ledger.rs:9-14,26-60`; `crates/aft/src/db/mod.rs:460-473` |
| Retention | Bash terminal tasks 30 days; compression raw events 30 days, batches 500, mutation mutex budget 100 ms with 50 ms busy wait, count lock retry 10 ms, skipped sweep retry after 5 s; ledger 7 days; named checkpoints 14 days. | `crates/aft/src/db/bash_tasks.rs:12`; `crates/aft/src/db/compression_events.rs:355-376`; `crates/aft/src/write_ledger.rs:16`; `crates/aft/src/checkpoint.rs:22-23` |
| gh-shim relay | Setup 5 s, then one 30 s budget (`RELAY_BUDGET`) for the whole exchange: every attempt is sent with only the time left, transient refusals sleep 5 then 10 s only if the sleep ends before the budget does, one remint retry, same nonce throughout. On expiry: exit 86 named refusal when nothing was sent (a transient refusal stopped by the budget, or no time left to send), 87 outcome unknown when a sent request got no reply (ordinary upstream failure 1). | `crates/aft/src/gh_shim_relay_client.rs:33-50,475-530,587-706,820-829`; `crates/aft/src/gh_shim.rs:40-42,6422-6428` |
| LSP | Interactive request 8 s; initialize handshake ≤30 s (caller-owned shorter timeout accepted), shutdown request 5 s standalone; `idle.lsp_ttl_minutes` default 10, clamped 1–10; newly spawned unclaimed child orphan grace 10 s. | `crates/aft/src/lsp/client.rs:23-28,711-731,805-811`; `crates/aft/src/config.rs:33-36`; `crates/aft/src/lsp/child_registry.rs:23-27,42-49` |

## Nesting against §1/§2

| Inner AFT work / outer limit | Nests? |
|---|---|
| RouteBind 12 s vs daemon bind relay 12 s | **No margin**: AFT's own expiry is equal to relay deadline, so an error generated at 12 s cannot reliably arrive before the daemon's timeout; normal configure+ack must finish earlier (`subc/mod.rs:153-154,5317-5329,5358-5403`). |
| Plain-start warmup 10 s vs SDK 90 s route-open retry / daemon 12 s bind relay | **Yes in intended path**: ready:false refuses opens before bind; swap candidate 90 s runs behind serving incumbent, not inside bind (`subc/manifest.rs:356`; `subc/readiness.rs:45-54`; `subc/mod.rs:5317-5330`). |
| Bridge reload wait min(call budget,45 s) vs SDK 90 s retry and daemon 30 s drain | **Yes**: 45 s can cover a 30 s drain plus restart, and the whole call, including time inside `routeOpen`, retry sleeps and the single vanished-route resend, comes out of the caller's deadline; the request is sent with only what is left. `subc-transport.ts:1711,1764-1832,1915-1971`. |
| AFT drain window vs daemon 30 s drain | **Yes on normal supplied deadline**: uses the daemon's absolute deadline, caps erroneous longer notice at 120 s; streams end immediately and held bash detaches, but ordinary tools may exceed 30 s (`subc/drain.rs:1-70`; `subc/mod.rs:4292-4350`). |
| AFT shutdown vs daemon 25 s child cap | LSP 1.5 s + search flush wait 0.3 s **nest individually**; synchronous callgraph refresh flush has no shown total deadline, so **whole shutdown not proved to nest** (`lsp/manager.rs:37-39,2461-2474`; `main.rs:602-623`). |
| Inspect 120 s default (10–600 s), 30 min bash and 30 s LSP handshake vs 30 s default SDK call | **Not generally nested**: inspect plugin explicitly grants server budget + 30 s transport headroom; long bash plugin requests a larger timeout; ordinary 30 s SDK budget does not cover them (`commands/inspect.rs:28-73`; `packages/opencode-plugin/src/tools/inspect.ts:13,347-350`; `packages/aft-bridge/src/command-timeouts.ts:15-27`). |

## Violations / unresolved risks against §0

- **Global admissions**: process-wide cold-build 2 and warm-reload 4 permits cross target/root boundaries, contrary to per-target/per-connection only; shared executor workers and maintenance cap also serve multiple roots, though per-actor queues remain bounded (`cold_build_limiter.rs:6-17`; `commands/configure.rs:4295-4302`; `executor/mod.rs:118-188,1452-1469`).
- **No definite unnamed refusal identified in the examined admission paths**: maintenance backpressure and bind refusal have codes/messages (`crates/aft/src/executor/mod.rs:1452-1469,2555-2558`; `crates/aft/src/subc/mod.rs:6333-6355`); this is not a proof that every AFT refusal is named.

**Not located as a separate knob:** the brief's “45 s request-deadline cap” is the bridge's *reload-wait* ceiling (`subc-transport.ts:234-247`), not a general request-deadline cap: `client.request` receives what is left of the requested timeout or SDK default 30 s (`subc-transport.ts:1891-1894`). No independent LSP process-*spawn* timeout was identified; the initialize handshake has the 30 s bound (`lsp/client.rs:455,711-731`).
