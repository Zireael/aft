/**
 * aft_semantic_doctor — semantic search health report.
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

const SemanticDoctorParams = Type.Object({
  probe_provider: Type.Optional(
    Type.Boolean({
      description:
        "Send a probe embedding to check provider connectivity. Adds latency; off by default.",
    }),
  ),
});

function buildSemanticDoctorSections(payload: unknown, theme: Theme): string[] {
  const response = asRecord(payload);
  if (!response) return [theme.fg("muted", "No semantic health data.")];

  const status = asString(response.status) ?? "unknown";
  const summaryLine = asString(response.summary_line);

  // Use the summary line from Rust if available
  if (summaryLine) {
    return [summaryLine];
  }

  const sections: string[] = [
    `## Semantic Search Health: ${theme.fg(status === "healthy" ? "success" : "warning", status)}`,
  ];

  // Config
  const config = asRecord(response.config);
  if (config) {
    const backend = asString(config.backend) ?? "unknown";
    const model = asString(config.model) ?? "unknown";
    const dimensions = asNumber(config.dimensions) ?? "unknown";
    sections.push(`**Config:** ${backend} / ${model} (${dimensions}d)`);
  }

  // Index
  const index = asRecord(response.index);
  if (index) {
    const indexStatus = asString(index.status) ?? "unknown";
    const entryCount = asNumber(index.entry_count) ?? 0;
    sections.push(`**Index:** ${indexStatus} (${entryCount} entries)`);
  }

  // Metrics
  const metrics = asRecord(response.metrics);
  if (metrics) {
    const totalQueries = asNumber(metrics.total_queries) ?? 0;
    const p50 = asNumber(metrics.p50_latency_ms) ?? 0;
    const p95 = asNumber(metrics.p95_latency_ms) ?? 0;
    sections.push(
      `**Metrics:** ${totalQueries} queries, p50=${p50.toFixed(0)}ms, p95=${p95.toFixed(0)}ms`,
    );
  }

  // Warnings
  const warnings = asRecords(response.warnings);
  if (warnings.length > 0) {
    const warningStrings = warnings.map((w) => asString(w) ?? "unknown").join(", ");
    sections.push(theme.fg("warning", `**Warnings:** ${warningStrings}`));
  }

  // Suggestions
  const suggestions = asRecords(response.suggestions);
  if (suggestions.length > 0) {
    sections.push("**Suggestions:**");
    for (const s of suggestions) {
      const label = asString(s.label) ?? "unknown";
      const message = asString(s.message) ?? "";
      sections.push(`- ${label}: ${message}`);
    }
  }

  return sections;
}

function renderSemanticDoctorCall(
  _args: Static<typeof SemanticDoctorParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  return renderToolCall("semantic_doctor", theme.fg("toolOutput", "health check"), theme, context);
}

function renderSemanticDoctorResult(
  result: AgentToolResult<unknown>,
  _args: Static<typeof SemanticDoctorParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  if (context.isError) return renderErrorResult(result, "semantic_doctor failed", theme, context);
  return renderSections(
    buildSemanticDoctorSections(extractStructuredPayload(result), theme),
    context,
  );
}

export function registerSemanticDoctorTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_semantic_doctor",
    label: "semantic_doctor",
    description: [
      "Semantic search health report. Returns config, index status, metrics, provider connectivity, warnings, and suggestions.",
      "",
      "Use when: diagnosing semantic search issues, checking index health, verifying provider configuration, or after configuration changes.",
      "",
      "Set probe_provider=true to test provider connectivity (adds latency).",
    ].join("\n"),
    parameters: SemanticDoctorParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof SemanticDoctorParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = {};

      if (typeof params.probe_provider === "boolean") {
        req.probe_provider = params.probe_provider;
      }

      const response = await callBridge(bridge, "semantic_doctor", req, extCtx);

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
      const status = (response.status as string) ?? "unknown";
      const parts: string[] = [`## Semantic Search Health: ${status}`];

      const config = response.config as Record<string, unknown> | undefined;
      if (config) {
        const backend = (config.backend as string) ?? "unknown";
        const model = (config.model as string) ?? "unknown";
        const dimensions = (config.dimensions as number) ?? "unknown";
        parts.push(`**Config:** ${backend} / ${model} (${dimensions}d)`);
      }

      const index = response.index as Record<string, unknown> | undefined;
      if (index) {
        const indexStatus = (index.status as string) ?? "unknown";
        const entryCount = (index.entry_count as number) ?? 0;
        parts.push(`**Index:** ${indexStatus} (${entryCount} entries)`);
      }

      const metrics = response.metrics as Record<string, unknown> | undefined;
      if (metrics) {
        const totalQueries = (metrics.total_queries as number) ?? 0;
        const p50 = (metrics.p50_latency_ms as number) ?? 0;
        const p95 = (metrics.p95_latency_ms as number) ?? 0;
        parts.push(
          `**Metrics:** ${totalQueries} queries, p50=${p50.toFixed(0)}ms, p95=${p95.toFixed(0)}ms`,
        );
      }

      const warnings = response.warnings as string[] | undefined;
      if (Array.isArray(warnings) && warnings.length > 0) {
        parts.push(`**Warnings:** ${warnings.join(", ")}`);
      }

      const suggestions = response.suggestions as
        | Array<{ label: string; message: string }>
        | undefined;
      if (Array.isArray(suggestions) && suggestions.length > 0) {
        parts.push("**Suggestions:**");
        for (const s of suggestions) {
          parts.push(`- ${s.label}: ${s.message}`);
        }
      }

      return textResult(parts.join("\n"), response);
    },
    renderCall(args, theme, context) {
      return renderSemanticDoctorCall(args, theme, context);
    },
    renderResult(result, _options, theme, context) {
      return renderSemanticDoctorResult(result, context.args, theme, context);
    },
  });
}
