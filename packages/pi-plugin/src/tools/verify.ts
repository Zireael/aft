/**
 * aft_verify — suggest verification actions after changes.
 */

import type { AgentToolResult, ExtensionAPI, Theme } from "@earendil-works/pi-coding-agent";
import { type Static, Type } from "typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";
import {
  asNumber,
  asRecord,
  asRecords,
  asString,
  extractStructuredPayload,
  type RenderContextLike,
  renderErrorResult,
  renderSections,
  renderToolCall,
} from "./render-helpers.js";

const VerifyParams = Type.Object({
  files: Type.Optional(
    Type.Array(Type.String(), {
      description: "Specific files to verify. Omit to use session context.",
    }),
  ),
  session: Type.Optional(
    Type.Boolean({
      description:
        "Include all files changed in session (default: false). Requires `files` to be empty.",
    }),
  ),
  project_root: Type.Optional(
    Type.String({
      description: "Project root for context (optional).",
    }),
  ),
});

function buildVerifySections(payload: unknown, theme: Theme): string[] {
  const response = asRecord(payload);
  if (!response) return [theme.fg("muted", "No verification suggestions.")];

  const suggestions = asRecords(response.suggestions);
  if (suggestions.length === 0) {
    return [theme.fg("muted", "No verification suggestions available.")];
  }

  const sections: string[] = ["## Verification Suggestions"];

  for (const s of suggestions) {
    const action = asString(s.action) ?? "unknown";
    const confidence = asString(s.confidence) ?? "unknown";
    const reason = asString(s.reason) ?? "";
    const command = asString(s.command);

    let line = `- ${theme.fg("accent", action)} (${confidence}): ${reason}`;
    if (command) {
      line += `\n  Command: \`${command}\``;
    }
    sections.push(line);
  }

  const diagnostics = asNumber(response.diagnostics);
  if (diagnostics !== undefined && diagnostics > 0) {
    sections.push(`\nDiagnostics to check: ${diagnostics}`);
  }

  const likelyTests = asRecords(response.likely_tests);
  if (likelyTests.length > 0) {
    const testNames = likelyTests.map((t) => asString(t) ?? "unknown").join(", ");
    sections.push(`\nLikely tests: ${testNames}`);
  }

  const fileKinds = asRecords(response.file_kinds);
  if (fileKinds.length > 0) {
    sections.push("\n## File Classification");
    for (const fk of fileKinds) {
      const file = asString(fk.file) ?? "unknown";
      const kind = asString(fk.kind) ?? "unknown";
      sections.push(`- ${file}: ${kind}`);
    }
  }

  return sections;
}

function renderVerifyCall(
  _args: Static<typeof VerifyParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  return renderToolCall("verify", theme.fg("toolOutput", "changes"), theme, context);
}

function renderVerifyResult(
  result: AgentToolResult<unknown>,
  _args: Static<typeof VerifyParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  if (context.isError) return renderErrorResult(result, "verify failed", theme, context);
  return renderSections(buildVerifySections(extractStructuredPayload(result), theme), context);
}

export function registerVerifyTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_verify",
    label: "verify",
    description: [
      "Verify changes by suggesting verification actions for changed files.",
      "",
      "Returns suggestions for diagnostics, tests, linting, and type checking based on file types and mutation risk.",
      "",
      "Use after making changes to understand what verification steps are recommended.",
    ].join("\n"),
    parameters: VerifyParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof VerifyParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = {};

      if (params.files && Array.isArray(params.files) && params.files.length > 0) {
        req.files = params.files;
      }

      if (typeof params.session === "boolean") {
        req.session = params.session;
      }

      if (typeof params.project_root === "string" && params.project_root.length > 0) {
        req.project_root = params.project_root;
      }

      const response = await callBridge(bridge, "verify", req, extCtx);

      // Use Rust's text if available, otherwise format the response
      const body = response.text as string | undefined;
      if (typeof body === "string") {
        return textResult(body, response);
      }

      // Build a readable summary from the structured response
      const suggestions = asRecords(response.suggestions);
      const diagnostics = asNumber(response.diagnostics);
      const likelyTests = asRecords(response.likely_tests);
      const fileKinds = asRecords(response.file_kinds);

      const parts: string[] = [];

      if (suggestions.length > 0) {
        parts.push("## Verification Suggestions");
        for (const s of suggestions) {
          const action = asString(s.action) ?? "unknown";
          const confidence = asString(s.confidence) ?? "unknown";
          const reason = asString(s.reason) ?? "";
          const command = asString(s.command);

          let line = `- **${action}** (${confidence}): ${reason}`;
          if (command) {
            line += `\n  Command: \`${command}\``;
          }
          parts.push(line);
        }
      }

      if (diagnostics !== undefined && diagnostics > 0) {
        parts.push(`\nDiagnostics to check: ${diagnostics}`);
      }

      if (likelyTests.length > 0) {
        const testNames = likelyTests.map((t) => asString(t) ?? "unknown").join(", ");
        parts.push(`\nLikely tests: ${testNames}`);
      }

      if (fileKinds.length > 0) {
        parts.push("\n## File Classification");
        for (const fk of fileKinds) {
          const file = asString(fk.file) ?? "unknown";
          const kind = asString(fk.kind) ?? "unknown";
          parts.push(`- ${file}: ${kind}`);
        }
      }

      if (parts.length === 0) {
        return textResult(
          "No verification suggestions available. Provide files or enable session mode.",
          response,
        );
      }

      return textResult(parts.join("\n"), response);
    },
    renderCall(args, theme, context) {
      return renderVerifyCall(args, theme, context);
    },
    renderResult(result, _options, theme, context) {
      return renderVerifyResult(result, context.args, theme, context);
    },
  });
}
