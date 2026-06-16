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
 * aft_semantic_eval — run a local JSONL eval suite against semantic search.
 *
 * Reports recall@k and MRR metrics for semantic search quality evaluation.
 */
export function semanticEvalTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const evalTool: ToolDefinition = {
    description: [
      "Run a local JSONL eval suite against semantic search and report recall@k and MRR.",
      "",
      "The JSONL file should have one case per line with 'query' and 'expected_paths' fields.",
      "",
      "Use when: evaluating semantic search quality, benchmarking retrieval, or after configuration changes.",
    ].join("\n"),
    args: {
      path: arg(z.string().describe("Path to the JSONL eval file (required)")),
      top_k: arg(
        z
          .number()
          .int()
          .positive()
          .optional()
          .describe("Maximum number of results per query (default: 10)"),
      ),
      include_per_case: arg(
        z.boolean().optional().describe("Include per-case results in response (default: true)"),
      ),
    },
    execute: async (args, _context): Promise<string> => {
      if (typeof args.path !== "string" || args.path.trim().length === 0) {
        throw new Error("semantic_eval: invalid params: `path` must be a non-empty string");
      }

      const params: Record<string, unknown> = {
        path: args.path,
      };

      if (typeof args.top_k === "number" && args.top_k > 0) {
        params.top_k = args.top_k;
      }

      if (typeof args.include_per_case === "boolean") {
        params.include_per_case = args.include_per_case;
      }

      const response = await callBridge(ctx, _context, "semantic_eval", params);

      if (response.success === false) {
        const message =
          typeof response.message === "string" && response.message.length > 0
            ? response.message
            : "semantic_eval failed";
        const code =
          typeof response.code === "string" && response.code.length > 0 ? response.code : undefined;
        throw new Error(code ? `semantic_eval: ${code} — ${message}` : message);
      }

      // Use Rust's summary_line if available
      if (typeof response.summary_line === "string" && response.summary_line.length > 0) {
        return response.summary_line;
      }

      // Build a readable summary from the structured response
      const parts: string[] = [];

      const total = typeof response.total === "number" ? response.total : 0;
      const hits = typeof response.hits_in_top_k === "number" ? response.hits_in_top_k : 0;
      const recall = typeof response.recall_at_k === "number" ? response.recall_at_k : 0;
      const mrr = typeof response.mrr === "number" ? response.mrr : 0;
      const k = typeof response.k === "number" ? response.k : 10;

      parts.push(`## Semantic Eval Results`);
      parts.push(`**Total cases:** ${total}`);
      parts.push(`**Hits in top-${k}:** ${hits}`);
      parts.push(`**Recall@${k}:** ${recall.toFixed(3)}`);
      parts.push(`**MRR:** ${mrr.toFixed(3)}`);

      // Per-case results
      const cases = response.cases;
      if (Array.isArray(cases) && cases.length > 0) {
        parts.push("\n### Per-Case Results");
        for (const c of cases) {
          const index = typeof c.index === "number" ? c.index : "?";
          const query = typeof c.query === "string" ? c.query : "unknown";
          const firstHitRank = typeof c.first_hit_rank === "number" ? c.first_hit_rank : null;
          const hit = typeof c.hit === "boolean" ? c.hit : false;

          let line = `- Case ${index}: "${query}"`;
          if (hit && firstHitRank !== null) {
            line += ` — hit at rank ${firstHitRank}`;
          } else {
            line += ` — no hit`;
          }
          parts.push(line);
        }
      }

      if (parts.length === 0) {
        return JSON.stringify(response, null, 2);
      }

      return parts.join("\n");
    },
  };

  return {
    aft_semantic_eval: evalTool,
  };
}
