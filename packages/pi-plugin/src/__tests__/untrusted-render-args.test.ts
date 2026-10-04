/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import type { Component } from "@earendil-works/pi-tui";
import type { AftConfig } from "../config.js";
import { registerPiToolSurface, resolvePiToolSurface } from "../tool-registration.js";
import { makeContext, makeResult, mockTheme, renderToString } from "./render-test-helpers.js";
import { makeMockApi, makeMockBridge, makePluginContext } from "./tool-test-utils.js";

interface ParameterSchema {
  type?: string;
  anyOf?: unknown[];
}

function malformedArgsForField(schema: ParameterSchema): unknown {
  if (schema.type === "string") return 42;
  if (schema.type === "number" || schema.type === "integer") return "not a number";
  if (schema.type === "boolean" || schema.type === "array" || schema.type === "object") {
    return "wrong type";
  }
  // The tool schemas' unions accept strings, arrays, or objects; a number is
  // outside each of those unions and exercises the renderer's fallback path.
  return 42;
}

describe("untrusted Pi renderer arguments", () => {
  test("every registered renderer handles absent, partial, and wrongly typed arguments", () => {
    const config: AftConfig = { disabled_tools: [], bash: { background: true } };
    const { api, tools } = makeMockApi();
    const { bridge } = makeMockBridge(() => ({ success: true, text: "ok" }));
    registerPiToolSurface(api, makePluginContext(bridge, { config }), resolvePiToolSurface(config));

    const renderableTools = [...tools.values()].filter(
      (tool) => typeof tool.renderCall === "function" || typeof tool.renderResult === "function",
    );
    expect(renderableTools.length).toBeGreaterThan(0);

    for (const tool of renderableTools) {
      const schema = tool.parameters as {
        properties?: Record<string, ParameterSchema>;
      };
      const documentedFields = Object.entries(schema?.properties ?? {});
      expect(documentedFields.length).toBeGreaterThan(0);
      const cases: Array<{ label: string; args: unknown }> = [
        { label: "undefined", args: undefined },
        { label: "null", args: null },
        { label: "non-object", args: 42 },
        { label: "empty object", args: {} },
        ...documentedFields.map(([field, propertySchema]) => ({
          label: `wrong-typed ${field}`,
          args: { [field]: malformedArgsForField(propertySchema) },
        })),
      ];

      for (const { label, args } of cases) {
        const context = makeContext(args);
        if (tool.renderCall) {
          let output: string;
          try {
            output = renderToString(tool.renderCall(args, mockTheme, context) as Component);
          } catch (error) {
            throw new Error(`${tool.name} renderCall threw for ${label}: ${String(error)}`);
          }
          if (!output.trim()) throw new Error(`${tool.name} renderCall was empty for ${label}`);
        }

        if (tool.renderResult) {
          let output: string;
          try {
            output = renderToString(
              tool.renderResult(
                makeResult("renderer output", {}),
                { expanded: true, isPartial: false },
                mockTheme,
                context,
              ) as Component,
            );
          } catch (error) {
            throw new Error(`${tool.name} renderResult threw for ${label}: ${String(error)}`);
          }
          if (!output.trim()) throw new Error(`${tool.name} renderResult was empty for ${label}`);
        }
      }
    }
  });
});
