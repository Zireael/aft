import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";

const z = tool.schema;

type ToolArg = ToolDefinition["args"][string];

function arg(schema: unknown): ToolArg {
  return schema as ToolArg;
}

/**
 * aft_verify — suggest verification actions after changes.
 *
 * Suggests diagnostics, likely tests, lint/typecheck commands, and next actions
 * based on changed files and source-test links.
 */
export function verifyTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const verifyTool: ToolDefinition = {
    description: [
      "Verify changes by suggesting verification actions for changed files.",
      "",
      "Returns suggestions for diagnostics, tests, linting, and type checking based on file types and mutation risk.",
      "",
      "Use after making changes to understand what verification steps are recommended.",
    ].join("\n"),
    args: {
      files: arg(
        z
          .array(z.string())
          .optional()
          .describe("Specific files to verify. Omit to use session context."),
      ),
      session: arg(
        z
          .boolean()
          .optional()
          .describe(
            "Include all files changed in session (default: false). Requires `files` to be empty.",
          ),
      ),
      project_root: arg(z.string().optional().describe("Project root for context (optional).")),
    },
    execute: async (args, _context): Promise<string> => {
      const params: Record<string, unknown> = {};

      if (args.files && Array.isArray(args.files) && args.files.length > 0) {
        params.files = args.files;
      }

      if (typeof args.session === "boolean") {
        params.session = args.session;
      }

      if (typeof args.project_root === "string" && args.project_root.length > 0) {
        params.project_root = args.project_root;
      }

      const response = await callBridge(ctx, _context, "verify", params);

      if (response.success === false) {
        const message =
          typeof response.message === "string" && response.message.length > 0
            ? response.message
            : "verify failed";
        const code =
          typeof response.code === "string" && response.code.length > 0 ? response.code : undefined;
        throw new Error(code ? `verify: ${code} — ${message}` : message);
      }

      // Format the response for the agent
      const suggestions = response.suggestions;
      const diagnostics = response.diagnostics;
      const likelyTests = response.likely_tests;
      const fileKinds = response.file_kinds;

      const parts: string[] = [];

      if (typeof response.text === "string" && response.text.length > 0) {
        return response.text;
      }

      // Build a readable summary
      if (Array.isArray(suggestions) && suggestions.length > 0) {
        parts.push("## Verification Suggestions");
        for (const s of suggestions) {
          const action = typeof s.action === "string" ? s.action : "unknown";
          const confidence = typeof s.confidence === "string" ? s.confidence : "unknown";
          const reason = typeof s.reason === "string" ? s.reason : "";
          const command = typeof s.command === "string" ? s.command : undefined;

          let line = `- **${action}** (${confidence}): ${reason}`;
          if (command) {
            line += `\n  Command: \`${command}\``;
          }
          parts.push(line);
        }
      }

      if (typeof diagnostics === "number" && diagnostics > 0) {
        parts.push(`\nDiagnostics to check: ${diagnostics}`);
      }

      if (Array.isArray(likelyTests) && likelyTests.length > 0) {
        parts.push(`\nLikely tests: ${likelyTests.join(", ")}`);
      }

      if (Array.isArray(fileKinds) && fileKinds.length > 0) {
        parts.push("\n## File Classification");
        for (const fk of fileKinds) {
          const file = typeof fk.file === "string" ? fk.file : "unknown";
          const kind = typeof fk.kind === "string" ? fk.kind : "unknown";
          parts.push(`- ${file}: ${kind}`);
        }
      }

      if (parts.length === 0) {
        return "No verification suggestions available. Provide files or enable session mode.";
      }

      return parts.join("\n");
    },
  };

  return {
    aft_verify: verifyTool,
  };
}
