/**
 * aft_semantic_eval — run a local JSONL eval suite against semantic search.
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

const SemanticEvalParams = Type.Object({
  path: Type.String({
    description: "Path to the JSONL eval file (required)",
  }),
  top_k: Type.Optional(
    Type.Integer({
      minimum: 1,
      default: 10,
      description: "Maximum number of results per query (default: 10)",
    }),
  ),
  include_per_case: Type.Optional(
    Type.Boolean({
      default: true,
      description: "Include per-case results in response (default: true)",
    }),
  ),
});

function buildSemanticEvalSections(payload: unknown, theme: Theme): string[] {
  const response = asRecord(payload);
  if (!response) return [theme.fg("muted", "No eval results.")];

  // Use Rust's summary_line if available
  const summaryLine = asString(response.summary_line);
  if (summaryLine) {
    return [summaryLine];
  }

  const sections: string[] = ["## Semantic Eval Results"];

  const total = asNumber(response.total) ?? 0;
  const hits = asNumber(response.hits_in_top_k) ?? 0;
  const recall = asNumber(response.recall_at_k) ?? 0;
  const mrr = asNumber(response.mrr) ?? 0;
  const k = asNumber(response.k) ?? 10;

  sections.push(`**Total cases:** ${total}`);
  sections.push(`**Hits in top-${k}:** ${hits}`);
  sections.push(`**Recall@${k}:** ${recall.toFixed(3)}`);
  sections.push(`**MRR:** ${mrr.toFixed(3)}`);

  // Per-case results
  const cases = asRecords(response.cases);
  if (cases.length > 0) {
    sections.push("\n### Per-Case Results");
    for (const c of cases) {
      const index = asNumber(c.index) ?? "?";
      const query = asString(c.query) ?? "unknown";
      const firstHitRank = asNumber(c.first_hit_rank);
      const hit = c.hit === true;

      let line = `- Case ${index}: "${query}"`;
      if (hit && firstHitRank !== undefined) {
        line += ` — hit at rank ${firstHitRank}`;
      } else {
        line += ` — no hit`;
      }
      sections.push(line);
    }
  }

  return sections;
}

function renderSemanticEvalCall(
  args: Static<typeof SemanticEvalParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  return renderToolCall("semantic_eval", theme.fg("toolOutput", args.path), theme, context);
}

function renderSemanticEvalResult(
  result: AgentToolResult<unknown>,
  args: Static<typeof SemanticEvalParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  if (context.isError) return renderErrorResult(result, "semantic_eval failed", theme, context);
  return renderSections(
    buildSemanticEvalSections(extractStructuredPayload(result), theme),
    context,
  );
}

export function registerSemanticEvalTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_semantic_eval",
    label: "semantic_eval",
    description: [
      "Run a local JSONL eval suite against semantic search and report recall@k and MRR.",
      "",
      "The JSONL file should have one case per line with 'query' and 'expected_paths' fields.",
      "",
      "Use when: evaluating semantic search quality, benchmarking retrieval, or after configuration changes.",
    ].join("\n"),
    parameters: SemanticEvalParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof SemanticEvalParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      if (typeof params.path !== "string" || params.path.trim().length === 0) {
        throw new Error("semantic_eval: invalid params: `path` must be a non-empty string");
      }

      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = {
        path: params.path,
      };

      if (typeof params.top_k === "number" && params.top_k > 0) {
        req.top_k = params.top_k;
      }

      if (typeof params.include_per_case === "boolean") {
        req.include_per_case = params.include_per_case;
      }

      const response = await callBridge(bridge, "semantic_eval", req, extCtx);

      // Use Rust's text if available
      const body = response.text as string | undefined;
      if (typeof body === "string") {
        return textResult(body, response);
      }

      // Use summary_line from Rust
      const summaryLine = response.summary_line as string | undefined;
      if (typeof summaryLine === "string") {
        return textResult(summaryLine, response);
      }

      // Build a readable summary
      const total = (response.total as number) ?? 0;
      const hits = (response.hits_in_top_k as number) ?? 0;
      const recall = (response.recall_at_k as number) ?? 0;
      const mrr = (response.mrr as number) ?? 0;
      const k = (response.k as number) ?? 10;

      const parts: string[] = [
        "## Semantic Eval Results",
        `**Total cases:** ${total}`,
        `**Hits in top-${k}:** ${hits}`,
        `**Recall@${k}:** ${recall.toFixed(3)}`,
        `**MRR:** ${mrr.toFixed(3)}`,
      ];

      const cases = response.cases as
        | Array<{
            index: number;
            query: string;
            first_hit_rank: number | null;
            hit: boolean;
          }>
        | undefined;

      if (Array.isArray(cases) && cases.length > 0) {
        parts.push("\n### Per-Case Results");
        for (const c of cases) {
          let line = `- Case ${c.index}: "${c.query}"`;
          if (c.hit && c.first_hit_rank !== null) {
            line += ` — hit at rank ${c.first_hit_rank}`;
          } else {
            line += ` — no hit`;
          }
          parts.push(line);
        }
      }

      return textResult(parts.join("\n"), response);
    },
    renderCall(args, theme, context) {
      return renderSemanticEvalCall(args, theme, context);
    },
    renderResult(result, _options, theme, context) {
      return renderSemanticEvalResult(result, context.args, theme, context);
    },
  });
}
