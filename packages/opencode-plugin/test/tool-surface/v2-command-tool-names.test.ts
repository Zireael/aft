/**
 * OpenCode 2 registers AFT's command tool as `shell`, replacing the host's own
 * `shell`, and its companions as `shell_status`, `shell_watch`, `shell_kill`
 * and `shell_write`. OpenCode 1 and Pi keep `bash` and `bash_*`.
 *
 * These tests pin both halves: the V2 registration and everything an agent on
 * that host can read, and the V1 surface, byte for byte, against a fixture
 * captured from the registration as it was before the rename.
 */
import { describe, expect, test } from "bun:test";
import { mkdtempSync, readdirSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { extname, join, relative, resolve } from "node:path";
import {
  adaptToolError,
  BASH_TRANSPORT_DISPOSITION,
  DEFAULT_DISABLED_TOOLS,
  maybeAppendGrepSearchHint,
  translateConfigDocument,
} from "@cortexkit/aft-bridge";
import { type ToolDefinition, tool } from "@opencode-ai/plugin";
import { Effect } from "effect";
import ts from "typescript";

import type { AftConfig } from "../../src/config.js";
import {
  buildAftToolDefinitions,
  buildOpenCodeToolMap,
  registerAftTools,
  type V2ToolEditor,
} from "../../src/tool-registration.js";
import {
  projectV2Tool,
  type V2PermissionRequest,
  type V2ProviderTool,
} from "../../src/tools/definitions/v2.js";
import type { PluginContext } from "../../src/types.js";
import { createV2RuntimeConsumer } from "../../src/wakes/runtime-consumer.js";

const ALL_TOOLS_CONFIG: AftConfig = {
  tool_surface: "all",
  hoist_builtin_tools: true,
  backup: { enabled: true },
  bash: true,
  search_index: true,
  semantic_search: true,
};
const PROJECT = mkdtempSync(join(tmpdir(), "aft-v2-shell-"));
const LOCATION = { directory: PROJECT, project: { directory: PROJECT, canonical: PROJECT } };
const BASH_FAMILY = ["bash", "bash_status", "bash_watch", "bash_kill", "bash_write"];
const SHELL_FAMILY = ["shell", "shell_status", "shell_watch", "shell_kill", "shell_write"];

/**
 * Wording that names the command tool or a companion by its `bash` spelling.
 *
 * `bash.watch_sync_max_ms`-style config keys, `bash-<hex>` task IDs and
 * "python/node/bash REPLs" (the bash program, not the tool) stay legitimate.
 */
const BASH_TOOL_NAMING =
  /\bbash_(?:status|watch|kill|write)\b|\bbash tool\b|\bbash\(|\b(?:use|through|in|over|with|Foreground) bash\b/;

function resolved(config: AftConfig): AftConfig {
  const doc = structuredClone(config) as Record<string, unknown>;
  const translation = translateConfigDocument(doc, "window", "user");
  if (translation.errors.length > 0) throw new Error(translation.errors.join(", "));
  doc.disabled_tools ??= [...DEFAULT_DISABLED_TOOLS];
  return doc as AftConfig;
}

type Send = (command: string, params: Record<string, unknown>) => unknown;

function context(config: AftConfig, commandToolName?: "shell", send?: Send): PluginContext {
  const bridge = {
    send: async (command: string, params: Record<string, unknown> = {}) => {
      if (!send) throw new Error("this test must not reach a bridge");
      return send(command, params);
    },
  };
  return {
    pool: { getBridge: () => bridge },
    client: {},
    config,
    hashlineEffective: false,
    storageDir: join(PROJECT, ".storage"),
    ...(commandToolName ? { commandToolName } : {}),
  } as never;
}

/** The OpenCode 2 definitions, built the way the V2 entry builds them. */
function v2Definitions(config: AftConfig = ALL_TOOLS_CONFIG, send?: Send) {
  const resolvedConfig = resolved(config);
  return buildAftToolDefinitions(context(resolvedConfig, "shell", send), resolvedConfig);
}

async function registerV2(definitions: Record<string, ToolDefinition>) {
  const events: string[] = [];
  const added: V2ProviderTool[] = [];
  const host = {
    tool: {
      transform: (transform: (editor: V2ToolEditor) => void) =>
        Effect.sync(() =>
          transform({
            add: (definition) => {
              events.push(`add:${definition.name}`);
              added.push(definition);
            },
            remove: (name) => events.push(`remove:${name}`),
          }),
        ),
    },
  };
  await Effect.runPromise(registerAftTools(host, LOCATION, definitions) as Effect.Effect<void>);
  return { events, added };
}

function executionContext() {
  return {
    sessionID: "ses-v2",
    messageID: "msg-v2",
    id: "call-v2",
    agent: "build",
    progress: () => Effect.void,
  };
}

function schemaText(definition: V2ProviderTool): string {
  return JSON.stringify(tool.schema.toJSONSchema(definition.input, { io: "input" }));
}

describe("OpenCode 2 registers the command tool as shell", () => {
  test("shell and the four shell_* companions are registered, and no bash name is", async () => {
    const { added } = await registerV2(v2Definitions());
    const names = added.map((definition) => definition.name);

    for (const name of SHELL_FAMILY) expect(names).toContain(name);
    for (const name of BASH_FAMILY) expect(names).not.toContain(name);
  });

  test("the host's own shell is removed before AFT's shell is added", async () => {
    const { events } = await registerV2(v2Definitions());

    const removal = events.indexOf("remove:shell");
    expect(removal).toBeGreaterThanOrEqual(0);
    expect(removal).toBeLessThan(events.indexOf("add:shell"));
    // The companions are AFT's own names; the host has nothing under them.
    for (const name of SHELL_FAMILY.slice(1)) expect(events).not.toContain(`remove:${name}`);
  });

  test("the command tool declares the permission action the host evaluates, shell", async () => {
    const { added } = await registerV2(v2Definitions());
    const shell = added.find((definition) => definition.name === "shell");

    expect(shell?.options.permission).toBe("shell");
  });

  test("disabled_tools keeps the canonical bash names and removes the shell registrations", async () => {
    const { events, added } = await registerV2(
      v2Definitions({ ...ALL_TOOLS_CONFIG, disabled_tools: ["bash", "bash_status"] }),
    );
    const names = added.map((definition) => definition.name);

    expect(names).not.toContain("shell");
    expect(names).not.toContain("shell_status");
    expect(names).toContain("shell_watch");
    // Without AFT's shell there is nothing to replace the host's with.
    expect(events).not.toContain("remove:shell");
  });

  test("a command ask reaches the host's permission service as shell", async () => {
    const sends: Array<{ command: string; params: Record<string, unknown> }> = [];
    let round = 0;
    const definitions = v2Definitions(ALL_TOOLS_CONFIG, (command, params) => {
      sends.push({ command, params });
      round += 1;
      if (round === 1) {
        return {
          success: false,
          code: "permission_required",
          asks: [{ kind: "bash", patterns: ["rm -rf build"], always: ["rm *"] }],
        };
      }
      return { success: true, status: "completed", exit_code: 0, output: "done" };
    });
    const asked: V2PermissionRequest[] = [];
    const shell = projectV2Tool("bash", definitions.bash as ToolDefinition, LOCATION, {
      requestPermission: async (request) => {
        asked.push(request);
      },
    });

    await Effect.runPromise(shell.execute({ command: "rm -rf build" }, executionContext()));

    expect(asked.map((request) => request.permission)).toEqual(["shell"]);
    expect(sends.map((send) => send.params.command_tool_name)).toEqual(["shell", "shell"]);
  });

  test("a denied shell rule refuses the command before it runs", async () => {
    let probes = 0;
    const definitions = v2Definitions(ALL_TOOLS_CONFIG, () => {
      probes += 1;
      return {
        success: false,
        code: "permission_required",
        asks: [{ kind: "bash", patterns: ["rm -rf build"], always: ["rm *"] }],
      };
    });
    const shell = projectV2Tool("bash", definitions.bash as ToolDefinition, LOCATION, {
      requestPermission: async (request) => {
        if (request.permission === "shell") throw new Error("Permission denied: shell");
      },
    });

    await expect(
      Effect.runPromise(shell.execute({ command: "rm -rf build" }, executionContext())),
    ).rejects.toThrow("Permission denied: shell");
    // Only the probe that returned the ask reached the bridge; the retry that
    // would have run the command was never sent.
    expect(probes).toBe(1);
  });
});

describe("OpenCode 2 agent-visible text names shell, never bash_*", () => {
  test("no registered V2 tool description or argument description names a bash tool", async () => {
    const { added } = await registerV2(v2Definitions());

    const leaks = added.flatMap((definition) =>
      [definition.description, schemaText(definition)]
        .filter((text) => BASH_TOOL_NAMING.test(text))
        .map((text) => `${definition.name}: ${text.match(BASH_TOOL_NAMING)?.[0]}`),
    );
    expect(leaks).toEqual([]);
  });

  test("completion and still-running wakes name shell_status and shell_kill", async () => {
    const prompts: string[] = [];
    const statuses: string[] = [];
    const consumer = createV2RuntimeConsumer({
      session: {
        prompt: (input: { text: string }) =>
          Effect.sync(() => {
            prompts.push(input.text);
            return { id: "wake" };
          }),
        synthetic: (input: { text: string }) =>
          Effect.sync(() => {
            statuses.push(input.text);
            return { id: "status" };
          }),
      },
    } as never);
    await consumer.executeBash?.({
      name: "bash",
      input: { command: "sleep 1", background: true },
      context: {
        sessionID: "ses-wake",
        directory: PROJECT,
        worktree: PROJECT,
        effectAbort: new AbortController().signal,
        abort: new AbortController().signal,
        metadata: () => {},
        ask: async () => {},
        progress: () => Effect.void,
      },
      definition: { description: "probe", args: {}, execute: async () => "bash-1" },
    });
    const bridge = { getCwd: () => PROJECT, send: async () => ({ success: true }) } as never;

    await consumer.bridgeOptions.onBashCompletion?.(
      {
        type: "bash_completed",
        session_id: "ses-wake",
        task_id: "bash-1",
        status: "completed",
        exit_code: 0,
        command: "sleep 1",
        output_preview: "partial",
        output_truncated: true,
      } as never,
      bridge,
    );
    await consumer.bridgeOptions.onBashLongRunning?.(
      {
        type: "bash_long_running",
        session_id: "ses-wake",
        task_id: "bash-2",
        command: "sleep 600",
        elapsed_ms: 60_000,
      } as never,
      bridge,
    );
    consumer.dispose();

    const texts = [...prompts, ...statuses];
    expect(texts.join("\n")).toContain("shell_status(");
    expect(statuses.join("\n")).toContain("shell_kill(");
    for (const text of texts) expect(text).not.toMatch(BASH_TOOL_NAMING);
  });

  test("the text the shell tools render from bridge replies names the shell tools", async () => {
    const definitions = v2Definitions(ALL_TOOLS_CONFIG, (command) => {
      if (command === "bash_status") {
        return { success: true, status: "running", mode: "pty", pty_screen: "$ " };
      }
      // Replies without a message fall back to text naming the tool.
      return { success: false };
    });
    const run = (name: string, input: Record<string, unknown>) =>
      Effect.runPromise(
        projectV2Tool(name, definitions[name] as ToolDefinition, LOCATION).execute(
          input,
          executionContext(),
        ),
      ).then(
        (result) => JSON.stringify(result),
        (error: unknown) => (error instanceof Error ? error.message : String(error)),
      );

    const texts = [
      await run("bash_status", { taskId: "bash-0123456789abcdef" }),
      await run("bash_kill", { taskId: "bash-0123456789abcdef" }),
      await run("bash_write", { taskId: "bash-0123456789abcdef", input: "q" }),
      await run("bash_watch", { taskId: "bash-0123456789abcdef", background: true }),
    ];

    expect(texts[0]).toContain("shell_status(");
    expect(texts[1]).toBe("shell_kill failed");
    expect(texts[2]).toBe("shell_write failed");
    expect(texts[3]).toContain("shell_watch without pattern");
    for (const text of texts) expect(text).not.toMatch(BASH_TOOL_NAMING);
  });

  test("the shared bridge hints name the shell tools when given the shell names", () => {
    const hinted = maybeAppendGrepSearchHint("a.ts:1:x", "grep -rn x src", true, PROJECT, "shell");
    expect(hinted).toContain("running grep/rg in shell");
    expect(hinted).not.toMatch(BASH_TOOL_NAMING);

    const error = Object.assign(new Error("bridge transport timed out"), {
      code: "transport_timeout",
    });
    const adapted = adaptToolError("bash", error, "shell_status") as Error;
    expect(adapted.message).toContain("Do not poll shell_status for it.");
    expect(adapted.message).not.toMatch(BASH_TOOL_NAMING);
  });

  /**
   * Every string and template literal in the source the OpenCode 2 path runs,
   * checked for `bash_*` wording. A literal that is exactly a wire command name
   * (`"bash_status"` sent to the bridge) or a V1 default argument
   * (`"bash_status"` passed to `bashTransportDisposition`) is not agent text.
   */
  test("no literal on the V2 source path spells a bash tool name into agent text", () => {
    const pluginRoot = resolve(import.meta.dir, "../..");
    const bridgeRoot = resolve(pluginRoot, "../aft-bridge/src");
    const files = [
      ...sourceFiles(resolve(pluginRoot, "src/tools")),
      ...sourceFiles(resolve(pluginRoot, "src/wakes")),
      ...sourceFiles(resolve(pluginRoot, "src/permissions")),
      resolve(pluginRoot, "src/tool-registration.ts"),
      resolve(pluginRoot, "src/config-error-surface.ts"),
      resolve(pluginRoot, "src/entry/server-runtime.mjs"),
      resolve(bridgeRoot, "bash-hints.ts"),
      resolve(bridgeRoot, "error-contract.ts"),
      resolve(bridgeRoot, "bash-host-fallback.ts"),
      resolve(bridgeRoot, "command-tool-names.ts"),
    ];
    const WIRE_NAMES = new Set(["bash", ...BASH_FAMILY, "bash_notify", "bash_regex_match"]);

    const leaks = files.flatMap((file) =>
      literals(file)
        .filter((text) => !WIRE_NAMES.has(text) && BASH_TOOL_NAMING.test(text))
        .map((text) => `${relative(pluginRoot, file)}: ${text.slice(0, 120)}`),
    );
    expect(leaks).toEqual([]);
  });
});

describe("OpenCode 1 keeps bash and bash_* byte-identical", () => {
  /**
   * The fixture was captured from the V1 registration before the rename, so
   * any change to what an OpenCode 1 agent is shown for these tools, or to
   * their names, fails here.
   */
  test("the V1 bash family and delete descriptions and schemas match the pre-rename capture", () => {
    const config = resolved(ALL_TOOLS_CONFIG);
    const v1 = buildOpenCodeToolMap(context(config), config);
    const fixture = JSON.parse(
      readFileSync(resolve(import.meta.dir, "fixtures/v1-bash-family.json"), "utf8"),
    ) as Record<string, { description: string; schema: unknown }>;

    const current = Object.fromEntries(
      [...BASH_FAMILY, "aft_delete"].map((name) => {
        const definition = v1[name] as ToolDefinition;
        return [
          name,
          {
            description: definition.description,
            schema: tool.schema.toJSONSchema(tool.schema.object(definition.args), { io: "input" }),
          },
        ];
      }),
    );
    expect(Object.keys(v1)).toEqual(expect.arrayContaining(BASH_FAMILY));
    for (const name of SHELL_FAMILY) expect(Object.keys(v1)).not.toContain(name);
    expect(current).toEqual(fixture);
  });

  test("the shared hints keep their bash wording when no host name is given", () => {
    expect(maybeAppendGrepSearchHint("a.ts:1:x", "grep -rn x src", true, PROJECT)).toBe(
      "a.ts:1:x\n\nDO NOT search code by running grep/rg in bash \u2014 it is unindexed, unranked, and serial. Use the `aft_search` tool instead (it auto-routes concepts, identifiers, regex, and literals).",
    );
    expect(BASH_TRANSPORT_DISPOSITION).toBe(
      "The transport to the AFT daemon was interrupted; no background task was created for this command and no task ID exists. Re-run the command. Do not poll bash_status for it.",
    );
  });
});

function sourceFiles(directory: string): string[] {
  return readdirSync(directory).flatMap((entry) => {
    const path = join(directory, entry);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return [".ts", ".mjs"].includes(extname(entry)) ? [path] : [];
  });
}

/** Every string and template-literal fragment in one source file. */
function literals(file: string): string[] {
  const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  const found: string[] = [];
  const visit = (node: ts.Node): void => {
    // Module specifiers such as "./bash_watch.js" are file names, not text.
    if (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) return;
    if (
      ts.isStringLiteral(node) ||
      ts.isNoSubstitutionTemplateLiteral(node) ||
      ts.isTemplateHead(node) ||
      ts.isTemplateMiddle(node) ||
      ts.isTemplateTail(node)
    ) {
      found.push(node.text);
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  return found;
}
