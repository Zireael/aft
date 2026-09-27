# Why a model on OpenCode 2 sends `{"$text": ...}` to bash_write (2026-09-27)

Trigger: the operator's VM drill on OpenCode 2.0.16 with **Space Bunny Free** through OpenCode Zen.
The agent called `bash_write` five to nine times with
`input: [{"key":"ctrl-u"},{"$text":"echo recovered\n"}]`, and once with `{"item": ...}`. Every call
came back as `Invalid arguments for tool "bash_write": - input: Invalid input`. Arrays of keys only,
and plain strings, worked.

Code references:

- AFT: this checkout (base `bea20611f`).
- OpenCode: `~/Work/OSS/opencode`. There is no local `v2.0.16` tag, so the 2.0.16 source is read at
  `ac2426e103` ("sync release versions for v2.0.16"). The 2.0.11 source is read at tag `v2.0.11`.

## TL;DR

- **The hypothesis is refuted for OpenCode.** OpenCode 2.0.11 and 2.0.16 send our `bash_write`
  schema to an OpenAI-compatible endpoint almost unchanged. The nested `anyOf`, including the bare
  `{"type":"string"}` arm inside `items`, reaches the wire as written. Space Bunny Free goes through
  that same OpenAI-compatible protocol. No sanitizer applies to it: OpenCode picks one only for
  Gemini or Kimi model names, an explicit `compatibility.sanitizer`, or the Gemini protocol, and
  neither sanitizer wraps a string arm in an object. The Zen gateway source in the same repo passes
  `tools[].function.parameters` through unchanged, even when it converts between wire formats.
- A mixed array **is** accepted end to end. `[{"key":"ctrl-u"},"fixture input\n"]` went through
  OpenCode 2.0.11, OpenCode 2.0.16 and the V1 host 1.18.30, and wrote 15 bytes. The union works, so
  the invented shapes come from the model.
- The keys `$text` and `item` look like an XML↔JSON mapping (a key for a text node, and an element
  name for an array entry). They are not in anything OpenCode or Zen sends. The leading explanation is
  the model's own habits when it serialises tool calls, or a hidden layer upstream of Zen (the
  anonymous provider behind Space Bunny) that we cannot see. Neither can be checked from this
  harness. What the harness does establish is that nothing on our side of `opencode.ai/zen/v1`
  rewrote the schema.
- The error is OpenCode's own. It comes from `formatInputIssues` in
  `packages/core/src/tool/runtime.ts`, which runs **before** AFT sees the call
  (`providerCall.executed: false`). The formatter prints only each top-level issue's `path` and
  `message`. Zod reports a failed union as one `invalid_union` issue at the union's own path
  (`input`) with the message `Invalid input`. The detail (`[1]`, missing `key`) sits in the issue's
  nested `errors` array, and the formatter never reads it. The full text also echoes the arguments
  and ends with "Update the arguments and call the tool again.", but it never names the bad index or
  says what shape was expected.
- We **can** control that message. A zod `error` function on the outer `input` union replaces
  `Invalid input` with any text we like, and the host prints it verbatim. A probe produced
  `input[1] must be a plain string (text) or { key: "<name>" } (a named key); got an object with keys $text`.
- `bash_write` is the only AFT tool with an `anyOf` inside array `items`, and the only one whose
  union mixes a scalar and an object inside an array. The full list is below.

## 1. What OpenCode 2 sends for bash_write

### Method (E2E harness only)

- **Harness.** The OpenCode 2 Docker harness image `aft-e2e-opencode2-linux:872da6d1ffbd` was already
  on the machine: OpenCode 2.0.11, V1 host 1.18.30, AFT and plugin built from checkout `d2866dcc`.
  The image was run directly with `AFT_E2E_SCENARIO=bash_write`.
- **Isolation.** Each row gets its own `HOME`, `TMPDIR` and XDG roots under the artifact root. Each
  host is its own `opencode run --standalone`, which spawns `serve --stdio --port 0`. The live
  OpenCode store was never touched.
- **Probe arguments.** `tests/docker/opencode2/scenarios/bash_write/registration.json` was
  bind-mounted read-only with the arguments changed. Row ids stayed the same, so the slice's own
  validation still passed.
  - `T1/happy`: `input: [{"key":"ctrl-u"},"fixture input\n"]`, expecting `bytes_written: 15`.
  - `T2/invalid_arguments`: `input: [{"key":"ctrl-u"},{"$text":"echo recovered\n"}]`.
  - `T2/missing_target`: `input: [{"key":"ctrl-u"},{"item":"echo recovered\n"}]`.
- **OpenCode 2.0.16.** A throwaway image derived from the harness image reinstalled
  `@opencode/cli@2.0.16` and `@opencode/cli-linux-x64@2.0.16` into `/opt/opencode`. In the container,
  `opencode --version` printed `opencode v2.0.16`. The same rows were run again. The harness's
  pinned-version contracts still say 2.0.11, because nothing checks the binary's version.
- **Capture.** The wire JSON is the aimock `mock-requests.json` the harness writes per row. It is
  the body of `POST /v1/chat/completions` as the provider received it.

The image built from `d2866dcc` registers the bash family as `shell`/`shell_*`, so the captured name
is `shell_write`. Its `input` schema is the same as `bash_write` at this checkout:
`subc_tool_schemas.json` has the same sha256 in the image and here, and `bash_write.ts` differs only
in the names used in its description strings.

Results: on 2.0.11, 4 of 4 rows passed. On 2.0.16, the first sweep ran 4 rows and 3 passed. The
fourth, `T2/invalid_arguments`, was a `host_startup_stall`: the model got 0 requests, which is the
known stall of the emulated x86 image. Rerun on its own, it passed.

### Captured tool definition (OpenCode 2.0.16, provider `@opencode/ai/providers/openai-compatible`)

```json
{
  "type": "function",
  "function": {
    "name": "shell_write",
    "description": "Write input bytes to a running PTY bash task. PTY-only; check shell_status reports mode: \"pty\" first. Input is either a string (verbatim bytes) or an array mixing strings and { key: \"esc\" | \"enter\" | \"up\" | \"ctrl-c\" | ... } objects for atomic text+key sequences such as [ \"iHello\", { key: \"esc\" }, \":wq\", { key: \"enter\" } ]. Named keys cover enter/return/tab/space/backspace/esc/escape, arrows, home/end/page-up/page-down/delete/insert, f1..f12, and ctrl-a..ctrl-z. Maximum 1 MiB per call (post-expansion).",
    "parameters": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "properties": {
        "taskId": { "description": "Background PTY task ID returned by shell({ pty: true, background: true }).", "type": "string" },
        "input": {
          "description": "Either a string of verbatim bytes (e.g. 'print(1)\\n') OR an array mixing strings and { key: '<name>' } objects for atomic text+key sequences. Example: [ 'iHello', { key: 'esc' }, ':wq', { key: 'enter' } ].",
          "anyOf": [
            { "type": "string" },
            {
              "type": "array",
              "items": {
                "anyOf": [
                  { "type": "string" },
                  {
                    "type": "object",
                    "properties": { "key": { "description": "Named control key, e.g. 'esc', 'enter', 'up', 'ctrl-c'. Case-insensitive.", "type": "string" } },
                    "required": ["key"],
                    "additionalProperties": false
                  }
                ]
              }
            }
          ]
        }
      },
      "required": ["taskId", "input"]
    },
    "strict": false
  }
}
```

Compared with `subc_tool_schemas.json`, only two things change:

- zod's `toJSONSchema(..., { io: "input" })` adds `additionalProperties: false` to the `{key}` arm.
- The OpenAI-chat lowering adds `strict: false`.

All 29 tool definitions in the 2.0.16 request are byte-identical to the 2.0.11 request. The V1 host
(1.18.30, T7 row) sends the same `input` schema under the name `bash_write`.

### Why the Zen path matches the harness path (source reading)

1. **Catalog.** Space Bunny Free is `opencode/space-bunny-free`, with `npm: "@ai-sdk/openai-compatible"`
   and `api: https://opencode.ai/zen/v1`. Source: models.dev catalog cache
   `~/.cache/opencode/models.json`, read only. Its only compatibility hint is
   `interleaved: { field: "reasoning_content" }`. `Model.compatibility()` (`packages/core/src/model.ts`
   ~275) turns that into `{ reasoningField }` only, with no `sanitizer`.
2. **Package mapping.** In `packages/core/src/aisdk-native.ts`, `native()` sees that provider
   `opencode` is not in `HOSTS`, so `@ai-sdk/openai-compatible` maps through `PACKAGES` to
   `@opencode/ai/providers/openai-compatible`. That is the package the harness's provider contract
   uses (`contract/host-provider-config.json`).
3. **Schema projection.** In `packages/ai/src/protocols/openai-chat.ts` (~831), each tool is lowered
   with `ToolSchemaProjection.modelCompatibility(tool.inputSchema, request.model)`. In
   `protocols/utils/tool-schema.ts`, the sanitizer is chosen in this order: an explicit
   `compatibility.sanitizer`, then the protocol default (only `gemini.ts` passes one), then the model
   name (`/gemini/i` or `/kimi/i`). `space-bunny-free` matches none of these, so the schema is
   returned untouched (`case undefined: return schema`).
4. **Strict mode.** `detectSupportsStrictMode` returns true for `opencode` and `opencode.ai`, so the
   tool gets `strict: false`. The provider is therefore not asked to constrain decoding to the
   schema.
5. **The sanitizers do not wrap scalars either.** `GeminiJsonSchema.normalize` (2.0.16) rewrites
   tuple `items`, draft-04 exclusive bounds, `required` names, and recursive `$ref`. The Moonshot
   projection adds `type` next to `enum` and turns tuple items into `anyOf`. Neither turns a string
   arm into an object. The 2.0.11 Gemini converter (`gemini-tool-schema.ts`) adds `type: "string"`
   only to array `items` that carry no schema intent, and our `items` has an `anyOf`.
6. **Input repair.** The input-repair plugin (`packages/core/src/plugin/tool-input-repair.ts`) runs
   on `execute.before` and does not touch this input. For an `anyOf` with no single candidate branch
   it returns the value as it was.
7. **Code Mode.** Code Mode does not apply. Our V2 projection sets `codemode: false`
   (`packages/opencode-plugin/src/tools/definitions/v2.ts` `projectV2Tool`). The model saw
   `bash_write` as a native function tool. That fits the error, which names `bash_write` directly and
   not `execute`.
8. **The Zen gateway.** The gateway lives in `packages/console/app/src/routes/zen/`.
   - `v1/chat/completions.ts` accepts `format: "oa-compat"`.
   - `util/handler.ts` sends the body through without conversion when the upstream format matches.
     Otherwise it converts through a common shape, and every converter copies the schema object
     unchanged: `provider/openai-compatible.ts` ~198-204 (`parameters: tool.parameters`),
     `provider/openai.ts` ~296-304, `provider/anthropic.ts` ~426-432
     (`input_schema: t.function.parameters`).
   - `provider/google.ts` `modifyBody` returns the body as it is.
   - Which upstream serves Space Bunny is Zen data, not code, and is not visible here. The deployed
     gateway may also differ from this source.

## 2. Where `input: Invalid input` comes from

- **Layer.** OpenCode core, `packages/core/src/tool/runtime.ts`. `execute` calls `decodeInput`,
  which calls `validateInput`. Our V2 tools hand OpenCode a zod object, a Standard Schema, so
  `validateStandard` runs zod's `~standard.validate`. On failure,
  `formatInputIssues(tool, issues, value)` builds the message. That file is the same in 2.0.11 and
  2.0.16. It runs before the plugin: the tool part's metadata shows
  `providerCall.executed: false`, and AFT never receives the call.
- **The formatter.** For at most 5 issues it prints `- <path>: <message>`, then
  `Arguments provided:` with the JSON, then `Update the arguments and call the tool again.`
- **Why it names no index or key.** Zod reports the failed outer union as **one** issue
  (`code: "invalid_union"`, `path: ["input"]`, `message: "Invalid input"`). The failures of each
  branch, including the inner item union at `path: [1]` and the missing `key`, are nested in
  `issue.errors`, and `formatInputIssues` ignores them. Probe output (zod 4.1.8, the version
  `@opencode-ai/plugin` bundles in the image):

  ```json
  [{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received array"}],[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}],[{"expected":"string","code":"invalid_type","path":["key"],"message":"Invalid input: expected string, received undefined"}]],"path":[1],"message":"Invalid input"}]],"path":["input"],"message":"Invalid input"}]
  ```

- **Full agent-visible text,** captured on 2.0.16 (`T2/invalid_arguments` rerun) and identical on
  2.0.11:

  ```
  Invalid arguments for tool "shell_write":
  - input: Invalid input

  Arguments provided:
  {
    "taskId": "bash-0000000000000000",
    "input": [
      { "key": "ctrl-u" },
      { "$text": "echo recovered\n" }
    ]
  }

  Update the arguments and call the tool again.
  ```

  The `{"item": ...}` variant gives the same text.

- **Side finding: extra keys are dropped silently.** The advertised `{key}` arm has
  `additionalProperties: false`, but zod's default object mode strips unknown keys; it does not
  reject them. `{"key":"enter","text":"ls"}` validates as `{"key":"enter"}`, and the `ls` is dropped
  without a word. If AFT's own check were ever reached, it would be no clearer: serde's untagged
  enums in `crates/aft/src/commands/bash_write.rs` produce
  `bash_write: invalid params: data did not match any variant of untagged enum BashWriteInput`.

## 3. Other AFT tools with union parameters

These are the `anyOf` nodes in the 2.0.16 wire capture (all 29 tools), checked against
`crates/aft/src/subc_tool_schemas.json`:

| Tool | Parameter | Union | Inside array `items`? |
|---|---|---|---|
| `bash_write` | `input` | `string \| array` | no (top level) |
| `bash_write` | `input[]` item | `string \| {key}` | **yes: the only one, and the only scalar/object mix inside items** |
| `bash_watch` | `pattern` | `string \| {regex}` | no. It mixes a scalar and an object, but at the top level |
| `aft_zoom` | `targets` | `{path,symbol} \| array<{path,symbol}>` | no. The items are one object type |
| `aft_zoom` | `symbols` | `string \| array<string>` | no |
| `aft_outline` | `target` | `string \| array<string>` | no |
| `aft_inspect` | `sections`, `scope` | `string \| array<string>` | no |

- **`edit`.** On OpenCode, `edit`'s `edits[]` item is **not** a union. It is one flat object with
  optional `oldString`, `newString`, `replaceAll`, `occurrence`, `startLine`, `endLine` and
  `content`, plus `additionalProperties: false`, and the mode is checked at runtime. That matches
  the flat item shape discussed in option (a2). In the zod sources, the unions in the table are
  declared in `packages/opencode-plugin/src/tools/bash_write.ts`, `bash_watch.ts` (~73),
  `reading.ts` (outline and zoom) and `inspect.ts`.
- **Pi, not exercised here.** Pi's TypeBox schemas have the same `bash_write` union
  (`packages/pi-plugin/src/tools/bash.ts` ~236) and the same `bash_watch` `pattern` union. They also
  have scalar/scalar `integer | string` unions from `optionalInt` (`tools/_shared.ts` ~44), including
  `edit`'s batch `startLine`/`endLine` inside `edits[]` items (see `__tests__/hoisted.test.ts` ~118).
  Those are unions inside items, but they never mix a scalar with an object.
- **Likely next.** `bash_watch`'s `pattern` is the closest to the failing shape. By the same
  reasoning, a model could send `{"$text": "..."}` or `{"pattern": "..."}` for it. It has not been
  observed.

## 4. Options (none built)

### (a) Flatter schema

- **(a1) `input: string` plus an optional `keys: string[]`** (or separate `before`/`after` key
  lists).
  - For: no union at all, and every model handles it.
  - Against: it loses interleaving. The vim idiom `"iHello", esc, ":wq", enter` needs text between
    keys in one atomic write, so it would take several calls or a key-token syntax inside the
    string. A token syntax is ambiguous with literal text, which is why named keys exist.
  - Breaking change for existing array callers, unless the old form stays accepted.
- **(a2) Items are always one flat object, `{ text?: string, key?: string }`,** with "exactly one"
  checked at runtime, like `edit`'s `edits[]`.
  - For: no `anyOf` inside `items`. The model's apparent instinct, wrapping every item in an object,
    becomes the correct shape, and `{"text": ...}` is the obvious key.
  - Against: the "exactly one" check moves to our side, so it needs a clear error there. Bare-string
    items must either stay accepted, which means keeping the union in the advertised schema, or be
    dropped, which breaks callers and the documented idiom in the tool description.
  - If bare strings stay accepted and only the object shape is advertised, host validation rejects
    strings before we see them. So advertising the flat shape means validating it ourselves or
    widening the host-visible schema.
- **(a3) `{text} | {key}` as two object arms.** This still leaves an `anyOf` inside `items`, which
  is the pattern we would be trying to remove. It is weaker than (a2) for no gain, except
  documentation that reads like a discriminated shape.

### (b) Accept common wrapper shapes at our boundary

- **The host validates first,** so anything lenient has to be in the zod schema the host runs.
- **`z.preprocess` is out.** It cannot be rendered as JSON Schema for `io: "input"`: zod throws on
  transforms, and `_shared.ts` records that this failure class crashes plugin load. The only way is
  to widen the advertised item schema, for example a loose object with optional
  `text`/`$text`/`item`/`value`/`key`, or `unknown`, and then normalise in `execute` or in Rust.
- **For:** the call succeeds on the first try.
- **Against:**
  - It is whack-a-mole: `$text` and `item` today, something else tomorrow.
  - A wider advertised schema invites more invented shapes from every model.
  - Guessing which key holds the text risks writing the wrong bytes into a live PTY. A shell
    command sent by mistake is not undoable.
  - It hides the model's error from the model, so it never learns the real shape.
- If this is chosen, accept only the exact keys we observed, and reject any object that has both
  `key` and a text alias.

### (c) Better error text (we do control it)

- **How.** Put a zod `error` function on the **outer** `input` union. It receives `iss.input` (the
  raw value), so it can find the first bad index itself and name the shape it expected.
  `formatInputIssues` prints `issue.message` verbatim.
- **Probe result** (same zod, same schema, only the error function added):
  `[{"path":["input"],"message":"input[1] must be a plain string (text) or { key: \"<name>\" } (a named key); got an object with keys $text"}]`.
- **Put it on the outer union.** A custom `error` on the inner item union alone has no effect,
  because its message stays nested under the outer `invalid_union` that the formatter never
  expands.
- **Cost:** a few lines, and no change to the schema or the wire.
- **Against:**
  - The model still spends one failed call. It only stops spending five to nine.
  - The explanation logic duplicates the schema and can drift from it.
  - It was proven at the zod layer and read from the host formatter's code, not run through the
    host end to end.
  - Other hosts format issues differently: the V1 host and Pi were not checked.
- **Also worth closing:** the silent key-stripping gap from section 2, by making the `{key}` arm
  `.strict()` so it matches the advertised `additionalProperties: false`.

### (d) Line reset for PTY writes (`clearLine`)

- **What.** An opt-in `clearLine: true`, or a documented `{ key: "ctrl-u" }` prefix idiom, that
  writes `\x15` (Ctrl-U) before the payload. That discards bytes an earlier unterminated write left
  in the line buffer.
- **Where it works:**
  - In canonical tty mode, `\x15` is VKILL and erases the pending line.
  - In bash readline it is `unix-line-discard`, which only kills **backward from the cursor**.
    `\x05\x15` (Ctrl-E, then Ctrl-U) is more robust, or `\x01\x0b` (Ctrl-A, then Ctrl-K).
  - In zsh, `^U` is `kill-whole-line`. In fish it is `backward-kill-line`.
  - In a Python or Node REPL it behaves like readline.
- **Where it is harmful.** In raw-mode programs it means something else: vim (insert mode deletes
  typed text; normal mode scrolls up), less (scrolls), and TUIs generally. So it must stay opt-in,
  not a default, and the description should say "for shell/REPL prompts only".
- **For:** one boolean covers the recovery the agent had to work out by itself, which is where the
  `{"key":"ctrl-u"}` in the failing calls came from.
- **Against:** it adds a mode with prompt-type assumptions. The array form can already express it
  as `[{"key":"ctrl-u"}, "echo recovered\n"]`, and the harness showed that shape works. A line in
  the description may be enough.

### Observations for choosing

- The union itself works on every host we can test. The failure is one model's serialisation
  habit, possibly helped by an upstream we cannot see.
- (c) is the cheapest change that directly addresses what was observed: repeated calls with an
  unhelpful error.
- (a2) removes the only `anyOf` inside `items` across AFT's schemas, at the cost of a schema
  migration.
- (b) trades a clearer contract for first-try success and carries a risk of wrong bytes.
- (d) is independent of the other three.

## Reproduction

1. Copy `tests/docker/opencode2/scenarios/bash_write/registration.json` and change the three rows'
   `arguments` as listed in section 1 (for `T1/happy`, also set `comparison.expected.bytes_written`
   to 15).
2. Run the harness image directly:

   ```
   docker run --rm --platform linux/amd64 --user "$(id -u):$(id -g)" \
     -v <artifacts>:/artifacts \
     -v <copy>:/workspace/tests/docker/opencode2/scenarios/bash_write/registration.json:ro \
     -e AFT_CHECKOUT_SHA=<git_sha from /opt/aft/bin/build-info.json in the image> \
     -e AFT_E2E_RUN_ID=<id> -e AFT_E2E_RUN_ROOT=/artifacts/<id> -e AFT_E2E_SCENARIO=bash_write \
     aft-e2e-opencode2-linux:<tag>
   ```

3. For 2.0.16, build a derived image first:

   ```
   FROM aft-e2e-opencode2-linux:<tag>
   USER root
   RUN npm install --prefix /opt/opencode --no-audit --no-fund \
         "@opencode/cli@2.0.16" "@opencode/cli-linux-x64@2.0.16"
   ```

4. Read the wire JSON from
   `<artifacts>/<id>/forensics/bash_write/T1/happy/mock-requests.json`: the request bodies that
   carry `tools`. Read the error text from the last request's `role: "tool"` message in the T2 rows.
