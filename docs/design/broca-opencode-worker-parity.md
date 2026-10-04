# Broca workers vs OpenCode workers: what AFT gives each

Status: **draft, updated for catalog presets.** The option-B implementation
(below) was stopped before any code landed and then superseded by the
preset decision recorded at the end. The parity table describes the code at
base `15e7f012`, before presets.

Sources: AFT at `15e7f012`; Broca at `118bfc2a`
(`~/Work/Projects/CortexKit/broca`); prefrontal at `3bb9f488b`; subc-protocol
`0.28.0` from the crates.io registry.

## Headline: Broca's tool calls run on legacy routes, as a head

Two facts decide almost every row below.

1. **Broca dispatches the model's tool calls on legacy routes, not v1.**
   Broca declares `role_versions: {"tool-provider": "v1"}` only on the routes
   fetch-plan admission opens to read the catalog and system text
   (`broca-subc/src/connection.rs:920` `route_open_tool_provider_v1`). The route
   that carries tool calls is opened through `route_open_single_attempt`
   (`connection.rs:899`), which passes `role_versions: None`; Broca's comment at
   `connection.rs:1043-1046` says so explicitly. AFT maps `None` to
   `RouteRole::Legacy` (`crates/aft/src/subc/tool_provider.rs:27-39`). So
   tool-provider v1's server-owned completion
   (`v1_server_completion_never_returns_a_promoted_or_background_launch`,
   `crates/aft/src/subc/bash.rs:1541`) never applies to a Broca model's bash
   call. Broca gets legacy behaviour: promotion after the foreground wait
   window, task ids, and `background: true` launches.
2. **AFT treats every Broca session as a head (primary) session.** The only
   input that makes a call a worker is the `worker_session` field next to the
   call in the subc request body (`ToolCallRequest.worker_session`,
   `crates/aft/src/subc/mod.rs:8390-8394`). Only AFT's own TS plugins set it.
   Broca sends `{name, arguments}` and never sets it, and the v1 decoder
   hard-codes `worker_session: false` (`subc/mod.rs:7189`). From there it
   reaches `RawRequest::worker_session()` (`crates/aft/src/protocol.rs:490-498`),
   the bash path (`subc/bash.rs:552-563`) and every other tool
   (`crates/aft/src/run_tool_call.rs:656-665`).

### Live evidence

This document was drafted from a worker session whose tool surface is AFT's
catalog verbatim: bare names (`status`, `bash_status`, `outline`, ...), no
`bash_watch`, and the catalog-only `bash_status` description ("... a
completion reminder arrives when it exits"). That makes it a catalog
consumer, so Broca, though nothing else in the session confirms the harness.
Its own bash calls got legacy behaviour with head wording:

```
Foreground bash didn't finish within 15s and was promoted to background: bash-8321933e0c1e632a. A completion reminder will be delivered automatically; use bash_status({ taskId: "bash-8321933e0c1e632a" }) to inspect output or bash_kill({ taskId: "bash-8321933e0c1e632a" }) to terminate.
```

```
Background task started: bash-c3f519a7efc5b304. A completion reminder will be delivered automatically; don't poll bash_status.
```

No completion reminder ever reaches a Broca worker. Promises like these are a
likely cause of the 9-in-88 runs ALF found where a Broca worker with a
background command polled `bash_status` or slept.

## Is there a worker signal AFT could read?

The route bind carries the daemon's scope stamp:
`ModuleControlRequest::RouteBind { scope: Option<ScopeStamp>, .. }`, which AFT
destructures at `subc/mod.rs:5979` and keeps in `RouteIdentityData.scope`
(`subc/mod.rs:1016-1024`). In subc-protocol 0.28.0, `ScopeStamp`
(`src/scope.rs:219`) has `kind: ScopeKind` with `Head | Worker | Ephemeral`
(`src/scope.rs:49-53`, serialized snake_case).

- **AFT never reads `kind` at this base.** The only consumer of the stamp is
  the gh-shim relay (`subc/mod.rs:6619-6630`), which uses owner and ref.
- **Who writes `Worker`:** prefrontal registers a `Worker` scope for each
  delegated task, including Broca-substrate tasks
  (`prefrontal-core-store/src/session_scope.rs:491`, test at `:829-833`).
- **Does it reach Broca's tool-call routes?** Broca passes a scope selector on
  a tool-call route open only "for a run admitted under a scope", and only
  when the daemon advertises `scopes/v1` (`connection.rs:1038-1042`,
  `subc_plane.rs:1182-1190`). The selector is independent of
  `role_versions`, so a scoped legacy route would carry the stamp. **Not
  verified live:** this task did not query the running daemon, so it has not
  confirmed that a Mason run on Broca is admitted under its worker scope, or
  that the installed daemon advertises `scopes/v1`.

To discuss: whether `ScopeStamp.kind == Worker` should mark a call as a worker
on any route, legacy included, or only on routes whose harness is Broca's.
Broca presents the harness `runner` to tool modules (`connection.rs:2668-2786`).
Applying it on every route would also change OpenCode sessions that run under
a worker scope but that the plugin treats as primary, because OpenCode's flag
comes from the parent-session check, not the scope. That is an OpenCode
behaviour change and is not made here.

## Parity table (worker session, this base)

"Broca" means the route Broca really uses for tool calls: legacy, with no
`worker_session`.

| Item | OpenCode worker | Broca worker | Verdict |
|---|---|---|---|
| Caller role | `worker_session: true` from the plugin's subagent check | always primary (no field; see headline) | different (gap) |
| Tool list: bash family | bash, bash_status, bash_watch, bash_kill, bash_write (companions need `bash.background`) | bash, bash_status, bash_kill, bash_write; **no bash_watch** (`subc/manifest.rs:399-409`) | different (gap) |
| Tool list: `status` | not an agent tool | `status` tool served | different (deliberate) |
| Tool list: powershell | not registered | served where `pwsh` resolves | different (deliberate) |
| Tool names | `aft_*` prefixed for AFT-only tools | bare names | different (deliberate; the gateway prefixes) |
| Schemas: arguments | from the plugin's Zod tools | generated from the same Zod tools (`packages/opencode-plugin/src/subc-tool-schemas.ts`) | same |
| Schemas: config-gated arguments | follow the project config (for example `sandbox` only when enabled) | fixed stub config, `sandbox` always declared (`subc-tool-schemas.ts:87-102`) | different (deliberate; one compiled catalog) |
| `bash` description | names bash_watch and the worker wait limit | watch-less variant: "a completion reminder arrives when it exits" | different (gap: false for a Broca worker) |
| `bash_status` description | "... To wait, use bash_watch." | "... a completion reminder arrives when it exits." (`subc-tool-schemas.ts:38-39`) | different (gap) |
| System text | none from AFT beyond the tool descriptions | Broca preset: "Use bash with wait: true for long commands. The provider waits for completion or the command deadline ..." plus a worker line when `params.worker` (`tool_provider.rs:136-172`) | different (gap: that sentence describes v1 server-owned completion, which Broca's tool-call routes don't use) |
| Promotion reply | "... It won't wake you when it finishes, so wait for it ..." plus the plugin's bash_watch note | "... A completion reminder will be delivered automatically ..." (`commands/bash_orchestrate.rs:71-80,162-177`) | different (gap) |
| Background launch reply | worker wording plus the bash_watch note | "... A completion reminder will be delivered automatically; don't poll bash_status." (`bash_orchestrate.rs:195-214`) | different (gap) |
| `bash_status` on a running task | "To wait for it, call bash_watch; don't poll." (plugin) | "A completion reminder will be delivered automatically; don't poll." (`subc_format.rs:1057`) | different (gap) |
| Kill-deadline sentence | worker variant: waits move the default kill | head variant: "unless you pass a longer `timeout`" (`bash_orchestrate.rs:108-113`) | different (gap) |
| Kill reason on a hard kill | "killed by AFT's default background limit of 30 minutes (exit 124)" | same Rust text | same |
| Repeat breaker | worker variant: "wait with a watch ... bash_watch" | head variant: "The turn must end now ..." (`response_finalize.rs:31-36`) | different (gap: ending the turn ends a worker) |
| `wait: true` | hands back at `bash.worker_wait_max_ms` (30 min), command keeps running | no worker cap (`subc/bash.rs:657-661`): blocks until the command ends or its 30-minute default kill fires | different (gap) |
| Kill renewal while waiting | each wait and each `bash_status` read renews a default kill | none (renewal needs a worker role, `commands/bash_status.rs:74`) | different (gap) |
| User-message detach | detaches `wait: true` on a new user message | Broca sends no `bash_wait_detach`; the description still promises it | different (gap in description) |
| Permission refusal | OpenCode permission prompt per command | none on a first-party bind; untrusted binds get a consumer elicitation | different (deliberate) |
| PTY | allowed (`bash.subagent_background` defaults true) | allowed, but `bash_write` is the only way to drive it and nothing waits on exit | same rules, weaker waiting |

## Decision record

- Ask 1 (from this task): bash_watch had nothing to watch on v1. That premise
  was wrong for tool calls. v1 server-owned completion only covers Broca's
  catalog-fetch routes; tool calls are legacy (headline fact 1).
- Parent decision "option B", agreed with BROCA (Broca's deadline rule lives
  in Broca's `crates/broca-subc/src/tool_deadline.rs`, not present in the
  checkout read here): on **v1 worker scopes**, keep a hand-back as the call's
  only result, add bash_watch, cap watches at 90 s until Broca ships its
  bash_watch deadline rule, and keep head and unscoped v1 routes on
  server-owned completion. **Superseded before implementation.** As specified
  it would not reach Broca's tool calls, which run on legacy routes (fact 1).
- **Decision (ALF, BROCA, SUBC): the role comes from r7.3 catalog presets, not
  from the scope kind.** Prefrontal's fetch plan names a preset per provider;
  Broca fetches AFT's catalog with it (`tool.catalog` `preset`). From
  subc-protocol 0.29, Broca carries the same preset on every tool call in a
  typed envelope field beside `call_key`. AFT now serves three presets:
  - `head` (also what an absent preset means): today's catalog, byte for byte,
    with the same fingerprint.
  - `worker`: the head tools plus `bash_watch`. `bash`, `powershell`,
    `bash_status` and `bash_watch` carry worker wording, generated by the same
    builders as the OpenCode tools (`subc_tool_presets.json`). The system text
    says how to wait (`wait: true`, `bash_watch`) and names the worker wait
    limit by its setting and default. PTY and `bash_write` stay, because the
    table above shows workers are allowed them.
  - `reader`: `read`, `grep`, `glob`, `search`, `outline`, `zoom`,
    `callgraph` and `inspect` only, with read-only system text.
- Each call's role resolves into `CallerRole` at one place
  (`tool_provider::resolve_caller_role`), from the call's
  `ToolCallRequest.preset` (subc-protocol 0.29). A known preset applies: a
  worker gets the worker wait cap and wording, and a reader is refused every
  non-reader tool by name. An unknown preset is refused by name. A call that
  names no preset is refused (`invalid_request`, field `preset`) on a route
  bound with a daemon scope stamp. On an unscoped route (the OpenCode and Pi
  plugins, Broca's legacy tool routes, `direct`) it gets
  `UNSCOPED_DEFAULT_PRESET` (`head`), or the plugins' `worker_session` role,
  exactly as before. Health counts presetless calls by harness
  (`dispatch_path.tool_call_presets`) and the scoped routes that made tool
  calls. Every route bind logs whether it carried a scope stamp.

## Recommendations (for Ufuk)

1. A Broca worker's calls get worker behaviour only once Broca sends
   `preset: "worker"` on each call. Until then they run as `head` on its
   unscoped legacy routes.
2. The worker preset serves `bash_watch`, but this commit does not implement
   it in the module. See the delivery notes for the open decision.
3. The head catalog's `bash` description still promises a completion reminder
   and user-message detach that Broca never delivers. Head is pinned byte for
   byte, so changing it is a separate, deliberate catalog change.
