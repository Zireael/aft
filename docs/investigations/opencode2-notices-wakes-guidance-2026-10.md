# OpenCode 2 notices, wakes and workflow guidance

## Source identity

Issues #394 and #395 were checked against AFT base
`a3bac10a34faca31823fcc27fc880164ba993020`, then tested with the fixes.
The requested `~/Work/OSS/opencode` checkout is **not** at 2.0.22: its HEAD is
`5716f8ba60e79ec60ec485b6e5291c0b0bc1f252` (2026-09-03, old `@opencode-ai`
scope). It was read, not modified. The authoritative host evidence below is
from the published **exact 2.0.22** packages, not that stale checkout or AFT's
2.0.11 development dependency.

Reproduce the host source with `npm pack @opencode/core@2.0.22
@opencode/plugin@2.0.22 @opencode/client@2.0.22 @opencode/schema@2.0.22`.
References below use paths **inside those tarballs**, starting at `package/`.
The core tarball SHA-256 is
`5b82c331bc2f87b9372cf2841dd78e6bd9e7020db964112aaf7c15bf8bb331d1`;
plugin is `0406af7b34d63cb6c1397ac1394bc632c28622f88c44f3584d00a45e2ab7f593`.
The emitted core chunks retain their original `// src/...` source labels.

## Confirmed and fixed

### #394 (1): informational messages

- **AFT before:** `packages/opencode-plugin/src/shared/ignored-message.ts:49-59`
  at the base commit uses the V1 `{path,body}` prompt and awaits an Effect as an
  ordinary value. There is also an independent copy of that V1 send logic in
  `src/notifications.ts:258-320`.
- **Host:** `@opencode/client/package/dist/effect/api/api.d.ts:308-322` specifies
  `{sessionID,text,...}` and an Effect for `prompt`; `:341-353` specifies
  `synthetic({sessionID,text,description?,metadata?,delivery?,resume?})`, also an
  Effect. The plugin exposes `synthetic` at
  `@opencode/plugin/package/dist/effect/session.d.ts:143-145`.
- **Runtime semantics:** `@opencode/core/package/dist/chunks/instruction-discovery-exnynncn.js:214-234`
  (`src/session/session.ts`) admits a synthetic inbox item and calls
  `execution.wake` only when `resume !== false`.
- **Fix:** recognize the Location-scoped V2 context and run
  `session.synthetic({sessionID,text,resume:false})` with `Effect.runPromise`.
  The notification copy delegates to this path only on V2; the V1 model/agent
  pinning and retry behavior is unchanged.
- **Caller audit:** restriction notice `src/tools/permissions.ts:131-140`;
  warning and announcement fallbacks `src/notifications.ts:417-486`;
  queued config-parse notices and configure chat delivery
  `src/notifications.ts:173-188,668-715`. The V2 boot's *session-less* startup
  warnings intentionally go to the log (`src/entry/server-runtime.mjs:54-60`),
  not a guessed session. Explicit-session warning APIs now run native Effects.

### #394 (2): retry and route recovery

- **AFT before:** base `src/wakes/session-delivery.ts:65-88` reserves task keys
  before admission and never releases them on failure. Base
  `src/wakes/runtime-consumer.ts:109-123,226-240` lets admission reject its
  callback and registers sessions only through `executeBash`.
- **Host:** the same prompt Effect contract above permits failure; session
  `prompt` hooks exist at `@opencode/plugin/package/dist/effect/session.d.ts:13-19,129-144`.
  The host actually invokes them at
  `@opencode/core/package/dist/chunks/instruction-discovery-36cgxzxf.js:45-58`
  (`src/session/prompt.ts`), with session ID, mutable prompt and metadata.
- **Fix:** reserve only newly admitted keys and release them on unsuccessful
  exits (including interruption/defects). Catch routing failures and retain the
  completion unacknowledged for the next registration, including failures of
  late delivery. Register from `bash_status`, `bash_watch`, and the native
  prompt hook as well as `bash`. Re-registering an existing route drains held
  failures rather than immediately returning. There is no tight retry loop.
- **Limit:** acknowledgement failures *after successful prompt admission* are
  not the prompt-failure bug; this change does not redesign that dedup/ack
  boundary or introduce a background retry scheduler.

### #394 (3): native tool and model hooks

- **AFT before:** base `src/index.ts:893-900,925-936,997-1008` records the read
  model, normalizes aliases and adds conflict guidance only in V1 hooks.
- **Host API:** `@opencode/plugin/package/dist/effect/tool.d.ts:20-46,55`
  exposes `execute.before` with mutable `input` and `execute.after` with a
  completed `result` or an error. `effect/session.d.ts:22-35,129-144` exposes
  the current `{providerID,id}` model on the session context hook.
- **Host order:** `@opencode/core/package/dist/chunks/instruction-discovery-w9p1c293.js:85-92,224-230`
  runs the before hook, then executes with `event.input`;
  `instruction-discovery-3w5fmr99.js:24-28,47-64` decodes that input before
  calling the tool. The after hook result is consumed at
  `instruction-discovery-w9p1c293.js:112-127`.
- **Fix:** register native hooks for AFT's registered tools only. Normalize
  aliases before decoding (preserving hashline bypass), reject invalid raw
  arguments as the host's tagged `Tool.Error` rather than letting unknown
  fields disappear, and modify only completed bash text for conflict guidance.
  A context hook remembers the current model. `src/shared/read-vision.ts:75-97`
  runs `provider.list()` as an Effect and supports the native data array and
  `capabilities.input` in addition to the V1 catalog.
- **Catalog evidence:** `@opencode/client/package/dist/effect/api/api.d.ts:1765-1774`
  returns `{location,data:Provider.Info[]}` as an Effect;
  `@opencode/schema/package/dist/model.d.ts:76-100,223-234` specifies the model
  reference and native capability input list.

### #395 (1): system guidance

- **AFT before:** base `src/index.ts:806-813,834-843` is the only injection:
  `experimental.chat.system.transform`. The native V2 entry registers tools,
  RPC and its prompt detach hook but no guidance hook.
- **Host:** `@opencode/plugin/package/dist/effect/session.d.ts:22-45,129-144`
  exposes `context` and `compaction`, both with mutable `system:SystemPart[]`.
  The host's own OpenCode tools push `SystemPart.make(text)` in those hooks at
  `@opencode/core/package/dist/chunks/instruction-discovery-hehaxqkv.js:69-75`.
- **Fix:** compute `buildHintsForRegisteredTools` after AFT's surface overrides
  and effective hashline decision, then register both native hooks and push
  `{type:"text",text:hintsBlock}`. The **text bytes** are the unchanged V1
  builder output. Log “Workflow hints injected” only after registration
  actually completes; missing hooks, empty guidance and failed registration
  do not claim injection.

## #395 (2), (3): verified, deliberately not changed

- AFT still registers `bash` and `apply_patch`; its replacement set is only
  `read/edit/write/apply_patch` (`src/tool-registration.ts:36,216-231`).
- The native tool names are `shell`
  (`@opencode/core/package/dist/chunks/instruction-discovery-0n82x6dt.js:59,186-188`)
  and `patch` (`instruction-discovery-qxmvdacm.js:88,117-120`). `add` overwrites
  only the *same* effective name and `remove(id)` deletes that entry
  (`instruction-discovery-w9p1c293.js:148-176`;
  `@opencode/plugin/package/dist/effect/tool.d.ts:7-18`). Thus registration does
  leave both pairs available.
- Legacy `tools.bash` and `permission.bash` normalize to `shell`
  (`instruction-discovery-1q5zb881.js:95-120`). Native shell requests that action
  (`instruction-discovery-0n82x6dt.js:148-156`). AFT uses `shell` for both its
  exposed permission option and translated bash asks
  (`src/tools/definitions/v2.ts:18,225-228,269-273`). Denying `shell` therefore
  removes/denies both shell tools, not just the builtin.
- **Qualification:** `patch` is not offered for every model. The native
  patch plugin's context/compaction/generate hooks remove `patch` unless the
  model ID contains `gpt-` and excludes `oss`/`gpt-4`; on that GPT branch they
  instead remove `edit`/`write`
  (`instruction-discovery-qxmvdacm.js:274-285`). The live GPT probe sees both
  `patch` and `apply_patch`; the initial `mock-model` probe did not see `patch`.
  Also, native `shell` already supports background commands (`Input` at
  `instruction-discovery-0n82x6dt.js:73-83`); AFT's compression, task companion
  tools and detach behavior are the differentiators, not background support
  alone.

Options for Ufuk (neither implemented):

1. **Register under `shell`/`patch` on V2.** Same-name replacement yields one
   tool per operation and native names in transcripts. Existing prompts,
   scripts, `disabled_tools`, AFT descriptions/schema artifacts and assumptions
   about `bash`/`apply_patch` names need coordinated adaptation. Permission
   actions remain `shell`/`edit`; the native model-based patch gate would need
   deliberate consideration.
2. **Remove the builtins while registering AFT's current names.** Preserves AFT
   names, companions, prompts and scripts but removes native command/patch
   behavior on the enabled AFT surface. Removal should be conditional on the
   replacement being enabled, so disabling AFT's tool does not unexpectedly
   delete the user's builtin fallback. The editor API supports this directly.

## Verification and live scope

- Each fixed behavior had a failing regression test before its fix, followed
  by a safely staged `NON-VACUITY BREAK` mutation and empty working diff after
  restoration. Each mutation failed exactly the named test; sibling tests
  either passed or were explicitly filtered out. Delivery metadata carries
  the individual mutation evidence.
- Exact **OpenCode 2.0.22**, native macOS arm64, was run with the committed
  `src/__tests__/helpers/live-v2-probe.ts`: a private HOME, all four XDG roots
  (plus runtime), a reserved service port written to isolated `service.json`,
  `--standalone`, and a loopback mock provider. Four real model requests prove
  the workflow block reaches the model, path aliases survive validation, bash
  receives the conflict hint, and both tool pairs coexist on `gpt-5-probe`.
  The restriction notice's native synthetic Effect completed and the text was
  read back from **only the throwaway** `opencode.db`, with `resume:false`.
- Live artifacts: `target/oc2-live-2qqcuu/{invocation.json,requests.json,stdout.log,stderr.log,synthetic-receipts.jsonl}`.
  Re-run after `bun run --cwd packages/opencode-plugin build` using
  `bun packages/opencode-plugin/src/__tests__/helpers/live-v2-probe.ts <exact-2.0.22-binary>`.
  The script records artifacts rather than deleting them. Earlier attempts
  exposed probe-only Effect import/PWD mistakes and the native patch model
  gate; they were corrected, not counted as product regressions.
- Prompt failure retry, route recovery after resume, model switches/catalog
  Effects, notification fallback callers, compaction guidance and registration
  log accuracy were verified from source plus regression/mutation tests, not
  claimed as live fault-injection observations.
- The repository's **2.0.11** Docker harness ran `bash/T5`: three scenarios
  passed; `completion_wake` cold-start-stalled before any model request or AFT
  plugin log. A targeted retry passed. Separate `bash_status/T1` and
  `bash_watch/T1` runs each passed one scenario. No startup budget or assertions
  were weakened. The first failed run remains recorded, not relabeled green.
  These runs tested the wake implementation at `8d1042e3`; the later raw-input
  rejection change does not alter wake delivery.
- Bun 1.4.2 full plugin suite: 1,895 passed, 3 skipped, 0 failed (1,898 tests,
  161 files). TypeScript 5.9.3 package typecheck and build passed. Biome 2.4.7
  root lint checked 680 files. Tool-schema generation covered 24 tools and
  left the committed schema artifacts unchanged; no public argument changed.
  Rust search/ranking/config files and package manifests/lockfiles were not
  changed.
