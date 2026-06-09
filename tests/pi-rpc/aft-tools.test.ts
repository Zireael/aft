import { describe, expect, test } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import {
  cleanupPiIsolatedEnv,
  createPiIsolatedEnv,
  type PiIsolatedEnv,
  type RpcClient,
  resolvePiPluginDir,
  spawnPiRpc,
  startAimock,
} from "./helpers";

async function enableAllToolSurface(env: PiIsolatedEnv): Promise<void> {
  await mkdir(join(env.workdir, ".pi"), { recursive: true });
  await writeFile(
    join(env.workdir, ".pi", "aft.jsonc"),
    JSON.stringify({ tool_surface: "all", search_index: true, semantic_search: false }),
    "utf8",
  );
}

function resultText(event: Record<string, unknown>): string {
  const result = event.result;
  if (result && typeof result === "object" && !Array.isArray(result)) {
    const content = (result as { content?: unknown }).content;
    if (Array.isArray(content)) {
      return content
        .map((part) => {
          if (!part || typeof part !== "object" || Array.isArray(part)) return "";
          const text = (part as { text?: unknown }).text;
          return typeof text === "string" ? text : "";
        })
        .filter((text) => text.length > 0)
        .join("\n");
    }
  }
  return JSON.stringify(result ?? "");
}

async function withPiTool(
  toolCall: { name: string; arguments: Record<string, unknown> },
  opts: {
    message: string;
    setup?: (env: PiIsolatedEnv) => Promise<void>;
  },
): Promise<Record<string, unknown>> {
  const env = createPiIsolatedEnv();
  const aimock = await startAimock();
  let client: RpcClient | undefined;
  try {
    await opts.setup?.(env);
    aimock.registerToolCallFixture({
      predicate: () => true,
      toolCalls: [toolCall],
      followupText: "Done.",
    });
    const spawned = spawnPiRpc({
      mockProviderURL: aimock.url,
      aftPluginDir: resolvePiPluginDir(),
      configDir: env.configDir,
      workdir: env.workdir,
    });
    client = spawned.client;
    expect(spawned.child.pid).toBeGreaterThan(0);
    expect((await client.sendCommand({ type: "prompt", message: opts.message })).success).toBe(
      true,
    );
    return await client.waitForEvent(
      (event) => event.type === "tool_execution_end" && event.toolName === toolCall.name,
      30_000,
    );
  } finally {
    await client?.close();
    await aimock.close();
    await cleanupPiIsolatedEnv(env);
  }
}

describe("AFT Pi tools (real Pi RPC)", () => {
  test("aft_callgraph trace_to_symbol returns a reachable path", async () => {
    const toolEnd = await withPiTool(
      {
        name: "aft_callgraph",
        arguments: {
          op: "trace_to_symbol",
          filePath: "trace.ts",
          symbol: "source",
          toSymbol: "target",
          toFile: "trace.ts",
        },
      },
      {
        message: "Trace source to target in trace.ts.",
        setup: async (env) => {
          await enableAllToolSurface(env);
          await writeFile(
            join(env.workdir, "trace.ts"),
            [
              "export function source(): string { return middle(); }",
              "function middle(): string { return target(); }",
              "export function target(): string { return 'target'; }",
              "",
            ].join("\n"),
            "utf8",
          );
        },
      },
    );

    expect(toolEnd.isError).toBe(false);
    const response = JSON.parse(resultText(toolEnd)) as {
      complete: boolean;
      path: Array<{ symbol: string }>;
    };
    expect(response.complete).toBe(true);
    expect(response.path.map((hop) => hop.symbol)).toEqual(["source", "middle", "target"]);
  }, 120_000);

  test("aft_outline files mode returns directory file metadata", async () => {
    const toolEnd = await withPiTool(
      {
        name: "aft_outline",
        arguments: {
          target: "src",
          files: true,
        },
      },
      {
        message: "Outline files under src.",
        setup: async (env) => {
          await mkdir(join(env.workdir, "src"), { recursive: true });
          await writeFile(
            join(env.workdir, "src", "one.ts"),
            "export function one() { return 1; }\n",
            "utf8",
          );
          await writeFile(join(env.workdir, "src", "two.py"), "def two():\n    return 2\n", "utf8");
        },
      },
    );

    expect(toolEnd.isError).toBe(false);
    const text = resultText(toolEnd);
    expect(text).toMatch(/one\.ts\s+typescript\s+1 syms\s+\d+ bytes/);
    expect(text).toMatch(/two\.py\s+python\s+1 syms\s+\d+ bytes/);
  }, 120_000);
});
