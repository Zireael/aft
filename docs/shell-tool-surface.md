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
| `bash` description (both) | foreground/auto-promote, `wait: true`, detach wording, `background: true`, `pty: true` | `bash.background` (else "Commands run in the foreground to completion") |
| `bash` description (both) | `bash_watch` wait steer, "never loop `bash_status`", PTY driven with `bash_status` / `bash_write` | each named companion registered (with `bash.background`); without `bash_watch` a sentence naming only the completion reminder (and `bash_status` if registered) |
| `bash` description (both) | detach wording variant | `bash.detach_on_user_message` |
| `bash` description (both) | code-search steer to `aft_search` or the `grep` tool; `aft_outline`; `aft_zoom` | `aft_search` / `aft_outline` / `aft_zoom` registered |
| `timeout` parameter (OpenCode) | promotion / `wait:true` wording | `bash.background` |
| `background` parameter | names `bash_status`, `bash_kill` (OpenCode) or `bash_watch`, `bash_status`, `bash_kill` (Pi) | present with `bash.background`; each name only when registered |
| `pty` parameter | names `bash_status`, `bash_write` | present with `bash.background`; each name only when registered |
| `pty` parameter | "Unavailable in subagent/worker sessions because bash.subagent_background is false." | `bash.subagent_background` false |
| Pi `promptSnippet` | "background tasks", "PTY mode" / "compressed output" | `bash.background` / `bash.compress` |
| Pi `promptGuidelines` | "Set compressed: false …"; the code-search steer | `bash.compress`; as the description steer |
| `bash_status` description | "To wait, use bash_watch." | `bash_watch` registered |
| `bash_watch` description | "Never loop bash_status to wait." | `bash_status` registered |
| `bash_write` description | "check bash_status reports mode: \"pty\" first" | `bash_status` registered |
| Companion descriptions | `bash({ background: true })` | registered only with `bash.background` |
| Workflow hint "Test/build output" | auto-compressed output | `bash` registered and `bash.compress` |
| Workflow hint "Long-running commands" | `wait: true`, `background: true`, `bash_watch`, `bash_status` | `bash.background`, `bash`, `bash_status` and `bash_watch` registered |
| Workflow hint "PTY / interactive commands" | `pty: true`, `bash_status`, `bash_write` | `bash.background`, `bash`, `bash_status` and `bash_write` registered |
| Workflow hint "Code exploration" | `aft_search` / `grep`, `aft_zoom` / `read` | those tools registered |
| Hashline rule (workflow hint and hashline `edit` description) | "(and accepted AFT `cat`/`head`/`tail` rewrites)" | `bash` registered and `bash.rewrite` |
| Hashline rule (workflow hint and hashline `edit` description) | `aft_zoom`, `aft_outline`, `aft_search` as tools that do not mint tags | each registered; `grep` is always named, since disabling AFT's grep leaves the host's grep |
| `aft_outline` description | "prefer aft_search + aft_zoom", "use aft_callgraph only for …" | `aft_search` / `aft_zoom` / `aft_callgraph` registered (`read` replaces `aft_zoom`) |

`bash`, `read` and `grep` are host slots: disabling AFT's registration leaves the host's own
tool under that name, so text naming them stays.

The matrix test also runs a `disabled_tools` dimension (each companion alone, `aft_search`,
`aft_zoom`, `aft_outline`, `aft_search`+`aft_zoom`, `aft_callgraph`; hashline on and off) and
scans every registered tool's description, parameter text, Pi prompt text and workflow hints
for an `aft_*` or companion name that is not registered.

## PTY in subagent sessions

Both plugins refuse `pty: true` in a subagent (Pi: worker) session only when
`bash.subagent_background` is false, because a PTY session only exists as a background task.
With the default (true) a subagent can start a PTY task and drive it with `bash_write` and
`bash_status` like a primary session.

## Module catalog (`crates/aft/src/subc_tool_schemas.json`)

The catalog is compiled into the binary and serves every project a subc consumer binds, so it
is one contract rather than a per-project surface. It is generated with compression,
background and the sandbox all on and every tool registered, because a consumer working in a
project that enables a feature must be able to see it. It has no `bash_watch`, and its `bash`
and `bash_status` descriptions do not mention one. Its `pty` description no longer says
"Unavailable in subagent sessions".

## Not covered here

- Descriptions that name `aft_safety` (backup and undo wording in `write`, `edit`,
  `apply_patch`, `aft_delete`, `aft_import`, `ast_grep_replace`) or `aft_inspect`
  (`apply_patch`) follow `backup.enabled` but not whether those tools are in
  `disabled_tools`.
- Runtime replies (for example a PTY start message or the subagent note naming `bash_watch`)
  are output, not descriptions, and are unchanged.
- Different wording for worker and head sessions, and the "end the turn" / "default 30s"
  wording.
