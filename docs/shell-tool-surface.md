# Shell tool surface: which feature each part depends on

A model sees only the shell parameters, tools and sentences whose feature is on. An option
that does nothing, or a description that points at a tool or parameter the model cannot
call, costs the model calls. This note lists every agent-facing part of the shell surface in
both plugins and the setting it follows. `shell-surface-matrix.test.ts`
(`packages/opencode-plugin/src/__tests__/`) checks every combination of `bash.compress`,
`bash.background` and `sandbox.enabled` for both plugins: the `bash` schema keys, the
registered companion names, and that no shell description, parameter text, prompt snippet
or workflow hint names an absent tool or parameter.

The feature flags are the resolved ones (`resolveBashConfig`; `sandbox.enabled === true`).
They are read when the tools are registered, so changing one takes effect on the next plugin
load. A tool named in `disabled_tools` is not registered, whatever its feature says.

## Parameters of `bash` (OpenCode and Pi)

| Parameter | Present when | Stale value while absent |
|---|---|---|
| `command`, `timeout`, `workdir`, `description` | always | n/a |
| `compressed` | `bash.compress` | forwarded as before; the engine ignores it while compression is off |
| `wait` | `bash.background` | ignored: the call is sent with `wait: false`, so it never errors and never becomes detachable into a background task |
| `background` | `bash.background` | ignored (already the case) |
| `pty`, `ptyRows`, `ptyCols` | `bash.background` | ignored (already the case) |
| `sandbox` | `sandbox.enabled` | forwarded as before; the engine acts on `sandbox: "host"` only while the sandbox is enabled |

With background off, a stale `wait: true` together with `background: true` or `pty: true`
used to fail the wait/background contradiction check. Those checks now only run while
background is on, so stale arguments never produce an error.

## Tools

| Tool | Registered when |
|---|---|
| `bash` | not in `disabled_tools` (`bash.enabled: false` keeps it; the engine answers `bash_disabled`) |
| `bash_status`, `bash_watch`, `bash_write`, `bash_kill` | `bash.background`, and not in `disabled_tools` |

The companions still register independently of `bash` itself. `bash_write` is PTY-only and
PTY needs `bash.background`, so it follows the same rule as the other three.

## Description and hint sentences

| Where | Sentence | Depends on |
|---|---|---|
| `bash` description (both) | "Output is compressed by default; pass compressed: false …", pipeline notes | `bash.compress` |
| `bash` description (both) | foreground/auto-promote, `wait: true`, detach wording, `background: true`, `pty: true`, `bash_status`, `bash_write`, `bash_watch` | `bash.background` (else "Commands run in the foreground to completion") |
| `bash` description (both) | detach wording variant | `bash.detach_on_user_message` |
| `bash` description (both) | code-search steer to `aft_search` or the `grep` tool; `aft_zoom` | `aft_search` / `aft_zoom` registered |
| `bash` description (OpenCode) | short waits go to `bash_watch` (else a sentence naming only `bash_status` and the completion reminder) | `bash_watch` registered |
| `timeout` parameter (OpenCode) | promotion / `wait:true` wording | `bash.background` |
| `background`, `pty` parameters | name `bash_status`, `bash_kill`, `bash_watch`, `bash_write` | present only with `bash.background` |
| Pi `promptSnippet` | "background tasks", "PTY mode" / "compressed output" | `bash.background` / `bash.compress` |
| Pi `promptGuidelines` | "Set compressed: false …" | `bash.compress` |
| Companion descriptions | `bash({ background: true })`, each other's names | registered only with `bash.background` |
| Workflow hint "Test/build output" | auto-compressed output | `bash` registered and `bash.compress` |
| Workflow hint "Long-running commands" | `wait: true`, `background: true`, `bash_watch`, `bash_status` | `bash.background`, `bash`, `bash_status` and `bash_watch` registered |
| Workflow hint "PTY / interactive commands" | `pty: true`, `bash_status`, `bash_write` | `bash.background`, `bash`, `bash_status` and `bash_write` registered |
| Workflow hint "Code exploration" | `aft_search` / `grep`, `aft_zoom` / `read` | those tools registered |
| Workflow hint "Hashline edit tags" | "(and accepted AFT `cat`/`head`/`tail` rewrites)" | `bash` registered and `bash.rewrite` |

## Module catalog (`crates/aft/src/subc_tool_schemas.json`)

The catalog is compiled into the binary and serves every project a subc consumer binds, so it
is one contract rather than a per-project surface. It is generated with compression,
background and the sandbox all on, because a consumer working in a project that enables a
feature must be able to see it. It has no `bash_watch`, and its `bash` and `bash_status`
descriptions do not mention one. This change leaves the catalog byte-identical.

## Not covered here

- Cross-references between tools removed through `disabled_tools`: `bash_status` still
  says "To wait, use bash_watch" when only `bash_watch` is disabled, the companions still
  mention `bash({ background: true })` when `bash` is disabled, and Pi's `bash` description
  names `bash_watch` even when that one tool is disabled.
- The hashline hint lists `aft_zoom`, `aft_outline`, `grep` and `aft_search` as tools that
  do not mint tags, whether or not they are registered.
- Different wording for worker and head sessions (PTY is still described to subagent
  sessions, which refuse it at call time), and the "end the turn" / "default 30s" wording.
