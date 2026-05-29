/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { __test__ } from "../index.js";
import { registerInspectTool } from "../tools/inspect.js";
import {
  executeTool,
  makeExtContext,
  makeMockApi,
  makeMockBridge,
  makePluginContext,
} from "./tool-test-utils.js";

describe("Pi aft_inspect surface", () => {
  test("registers at recommended surface unless explicitly disabled", () => {
    expect(__test__.resolveToolSurface({ tool_surface: "recommended" }).inspect).toBe(true);
    expect(__test__.resolveToolSurface({ tool_surface: "minimal" }).inspect).toBe(false);
    expect(
      __test__.resolveToolSurface({
        tool_surface: "recommended",
        disabled_tools: ["aft_inspect"],
      }).inspect,
    ).toBe(false);
    expect(
      __test__.resolveToolSurface({
        tool_surface: "recommended",
        inspect: { enabled: false },
      }).inspect,
    ).toBe(false);
  });
});

describe("Pi aft_inspect adapter", () => {
  test("declares constrained topK schema and Pi-specific async wording", () => {
    const { api, tools } = makeMockApi();
    const { bridge } = makeMockBridge(() => ({ success: true, summary: {} }));
    registerInspectTool(api, makePluginContext(bridge));

    const inspect = tools.get("aft_inspect")!;
    const description = inspect.description ?? "";
    expect(description).not.toContain("diagnostics");
    expect(description).not.toContain("triggered on session idle");
    expect(description).not.toContain("prewarm");
    expect(description).toContain("asynchronously on demand");
    expect(description).toContain("quietly starts a background Tier 2 warmup");
    expect(description).toContain("next call can use cached data");

    const parameters = inspect.parameters as {
      properties?: Record<string, Record<string, unknown>>;
    };
    expect(parameters.properties?.topK).toMatchObject({
      type: "integer",
      minimum: 1,
      maximum: 100,
      default: 20,
    });
  });

  test("sends corrected inspect field names to the bridge", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, summary: {} }));
    registerInspectTool(api, makePluginContext(bridge));

    await executeTool(
      tools.get("aft_inspect")!,
      { sections: "todos", scope: ["src", "tests"], topK: 9 },
      makeExtContext("/repo", "pi-session"),
    );

    expect(calls[0].command).toBe("inspect");
    expect(calls[0].params).toEqual({
      sections: "todos",
      scope: ["src", "tests"],
      topK: 9,
      session_id: "pi-session",
    });
  });

  test("normalizes empty sections and scope sentinels", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, summary: {} }));
    registerInspectTool(api, makePluginContext(bridge));

    await executeTool(
      tools.get("aft_inspect")!,
      { sections: [], scope: "" },
      makeExtContext("/repo", "pi-session"),
    );

    expect(calls[0].params.sections).toBeUndefined();
    expect(calls[0].params.scope).toBeUndefined();
    expect(calls[0].params.topK).toBeUndefined();
  });

  test("fires a quiet Tier 2 run when inspect returns cold pending categories", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge((command) => {
      if (command === "inspect_tier2_run") {
        return new Promise<Record<string, unknown>>(() => {});
      }
      return {
        success: true,
        summary: {},
        scanner_state: {
          stale_categories: [],
          pending_categories: ["dead_code", "unused_exports", "duplicates"],
        },
      };
    });
    registerInspectTool(api, makePluginContext(bridge));

    const result = (await executeTool(
      tools.get("aft_inspect")!,
      {},
      makeExtContext("/repo", "pi-session"),
    )) as { details: { scanner_state: { pending_categories: string[] } } };

    expect(result.details.scanner_state.pending_categories).toEqual([
      "dead_code",
      "unused_exports",
      "duplicates",
    ]);
    expect(calls).toHaveLength(2);
    expect(calls[0].command).toBe("inspect");
    expect(calls[1].command).toBe("inspect_tier2_run");
    expect(calls[1].params).toEqual({
      categories: ["dead_code", "unused_exports", "duplicates"],
      session_id: "pi-session",
    });
  });

  test("second inspect call can read cached Tier 2 data after the trigger starts", async () => {
    const { api, tools } = makeMockApi();
    let tier2RunStarted = false;
    const { bridge, calls } = makeMockBridge((command, params) => {
      if (command === "inspect_tier2_run") {
        tier2RunStarted = true;
        return { success: true, queued_categories: params.categories ?? [] };
      }
      if (tier2RunStarted) {
        return {
          success: true,
          summary: {
            dead_code: { count: 1 },
            unused_exports: { count: 2 },
            duplicates: { count: 3 },
          },
          scanner_state: {
            stale_categories: [],
            pending_categories: [],
          },
        };
      }
      return {
        success: true,
        summary: {},
        scanner_state: {
          stale_categories: [],
          pending_categories: ["dead_code", "unused_exports", "duplicates"],
        },
      };
    });
    registerInspectTool(api, makePluginContext(bridge));

    await executeTool(tools.get("aft_inspect")!, {}, makeExtContext("/repo", "pi-session"));
    const result = (await executeTool(
      tools.get("aft_inspect")!,
      {},
      makeExtContext("/repo", "pi-session"),
    )) as {
      details: {
        summary: { dead_code: { count: number }; unused_exports: { count: number } };
        scanner_state: { pending_categories: string[] };
      };
    };

    expect(result.details.scanner_state.pending_categories).toEqual([]);
    expect(result.details.summary.dead_code.count).toBe(1);
    expect(result.details.summary.unused_exports.count).toBe(2);
    expect(calls.map((call) => call.command)).toEqual(["inspect", "inspect_tier2_run", "inspect"]);
  });

  test("does not double-trigger while a Tier 2 run is already in flight", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge((command) => {
      if (command === "inspect_tier2_run") {
        return new Promise<Record<string, unknown>>(() => {});
      }
      return {
        success: true,
        summary: {},
        scanner_state: {
          stale_categories: [],
          pending_categories: ["dead_code", "unused_exports", "duplicates"],
        },
      };
    });
    registerInspectTool(api, makePluginContext(bridge));

    await executeTool(tools.get("aft_inspect")!, {}, makeExtContext("/repo", "pi-session"));
    await executeTool(tools.get("aft_inspect")!, {}, makeExtContext("/repo", "pi-session"));

    expect(calls.map((call) => call.command)).toEqual(["inspect", "inspect_tier2_run", "inspect"]);
    expect(calls[1].params.categories).toEqual(["dead_code", "unused_exports", "duplicates"]);
  });

  test("rejects invalid topK without coercion", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, summary: {} }));
    registerInspectTool(api, makePluginContext(bridge));

    await expect(
      executeTool(
        tools.get("aft_inspect")!,
        { sections: "todos", topK: "9" },
        makeExtContext("/repo", "pi-session"),
      ),
    ).rejects.toThrow("topK must be an integer between 1 and 100");
    await expect(
      executeTool(
        tools.get("aft_inspect")!,
        { sections: "todos", topK: 101 },
        makeExtContext("/repo", "pi-session"),
      ),
    ).rejects.toThrow("topK must be between 1 and 100");
    expect(calls).toHaveLength(0);
  });
});
