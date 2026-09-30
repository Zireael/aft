/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { registerHoistedTools } from "../tools/hoisted.js";
import {
  executeTool,
  makeExtContext,
  makeMockApi,
  makeMockBridge,
  makePluginContext,
} from "./tool-test-utils.js";

function registerReadHarness(ghReadEnabled = false) {
  const { api, tools } = makeMockApi();
  const { bridge, calls } = makeMockBridge(() => ({ success: true, text: "read result" }));
  registerHoistedTools(
    api,
    makePluginContext(bridge, { config: { github: { read: ghReadEnabled } } }),
    {
      hoistRead: true,
      hoistWrite: false,
      hoistEdit: false,
      hoistGrep: false,
      restrictToProjectRoot: true,
    },
  );
  return { tool: tools.get("read")!, calls };
}

function callArgs(calls: Array<{ params: Record<string, unknown> }>, index = 0) {
  return calls[index]?.params.arguments as Record<string, unknown>;
}

function withModel(input: string[] | undefined): ExtensionContext {
  return {
    ...makeExtContext(process.cwd(), `vision-${crypto.randomUUID()}`),
    ...(input === undefined ? {} : { model: { input } }),
  } as ExtensionContext;
}

describe("Pi read vision capability", () => {
  test("injects true for a vision-capable current model without exposing a schema field", async () => {
    const { tool, calls } = registerReadHarness();

    await executeTool(tool, { path: "issue://42" }, withModel(["text", "image"]));

    expect(callArgs(calls)).toEqual({ filePath: "issue://42", vision_capability: true });
    const parameters = tool.parameters as { properties?: Record<string, unknown> };
    expect(Object.keys(parameters.properties ?? {})).toEqual([
      "path",
      "startLine",
      "endLine",
      "limit",
      "offset",
    ]);
    expect(parameters.properties).not.toHaveProperty("vision_capability");
    expect(tool.description).toBe(
      "Read file contents with line numbers or sorted directory entries. Use startLine/endLine or 1-based offset/limit for either. Directory limit defaults to and is capped at 1000; enumeration stops at 10,000 entries and partial listings carry a shown/total trailer. Backed by AFT's indexed Rust reader — faster than the built-in `read` on large repos. Images are returned as attachments on vision-capable models; PDFs and non-vision models are not yet supported.",
    );
  });

  test("advertises GitHub resource spellings only when the user gate is enabled", () => {
    const enabled = registerReadHarness(true).tool.description;
    const disabled = registerReadHarness(false).tool.description;

    expect(enabled).toContain(
      "GitHub issues and pull requests can be read with `issue://NUMBER` and `pr://NUMBER`",
    );
    expect(disabled).not.toContain("issue://NUMBER");
  });

  test("injects false for a vision-less current model", async () => {
    const { tool, calls } = registerReadHarness();

    await executeTool(tool, { path: "issue://42" }, withModel(["text"]));

    expect(callArgs(calls)).toEqual({ filePath: "issue://42", vision_capability: false });
  });

  test("omits the internal field when the current model capability is unavailable", async () => {
    const { tool, calls } = registerReadHarness();

    await executeTool(tool, { path: "issue://42" }, withModel(undefined));

    expect(callArgs(calls)).toEqual({ filePath: "issue://42" });
  });

  test("uses the model currently on the extension context for each call", async () => {
    const { tool, calls } = registerReadHarness();
    const extCtx = withModel(["text", "image"]) as ExtensionContext & {
      model?: { input?: string[] };
    };

    await executeTool(tool, { path: "issue://42" }, extCtx);
    extCtx.model = { input: ["text"] };
    await executeTool(tool, { path: "issue://42" }, extCtx);

    expect(
      calls.map((call) => (call.params.arguments as Record<string, unknown>).vision_capability),
    ).toEqual([true, false]);
  });
});
