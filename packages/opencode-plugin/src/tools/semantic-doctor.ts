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
 * aft_semantic_doctor — semantic search health report.
 *
 * Produces a health report for the semantic search subsystem including:
 * - Config summary (backend, model, dimensions, etc.)
 * - Index status (ready, building, disabled, etc.)
 * - Metrics (query count, latency percentiles, error rates)
 * - Provider connectivity (optional probe)
 * - Warnings and suggestions
 */
export function semanticDoctorTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const doctorTool: ToolDefinition = {
    description: [
      "Semantic search health report. Returns config, index status, metrics, provider connectivity, warnings, and suggestions.",
      "",
      "Use when: diagnosing semantic search issues, checking index health, verifying provider configuration, or after configuration changes.",
      "",
      "Set probe_provider=true to test provider connectivity (adds latency).",
    ].join("\n"),
    args: {
      probe_provider: arg(
        z
          .boolean()
          .optional()
          .describe(
            "Send a probe embedding to check provider connectivity. Adds latency; off by default.",
          ),
      ),
    },
    execute: async (args, _context): Promise<string> => {
      const params: Record<string, unknown> = {};

      if (typeof args.probe_provider === "boolean") {
        params.probe_provider = args.probe_provider;
      }

      const response = await callBridge(ctx, _context, "semantic_doctor", params);

      if (response.success === false) {
        const message =
          typeof response.message === "string" && response.message.length > 0
            ? response.message
            : "semantic_doctor failed";
        const code =
          typeof response.code === "string" && response.code.length > 0 ? response.code : undefined;
        throw new Error(code ? `semantic_doctor: ${code} — ${message}` : message);
      }

      // Use Rust's summary_line if available
      if (typeof response.summary_line === "string" && response.summary_line.length > 0) {
        return response.summary_line;
      }

      // Build a readable summary from the structured response
      const parts: string[] = [];

      // Status
      const status = typeof response.status === "string" ? response.status : "unknown";
      parts.push(`## Semantic Search Health: ${status}`);

      // Config
      const config = response.config;
      if (config && typeof config === "object") {
        const backend = typeof config.backend === "string" ? config.backend : "unknown";
        const model = typeof config.model === "string" ? config.model : "unknown";
        const dimensions = typeof config.dimensions === "number" ? config.dimensions : "unknown";
        parts.push(`\n**Config:** ${backend} / ${model} (${dimensions}d)`);
      }

      // Index
      const index = response.index;
      if (index && typeof index === "object") {
        const indexStatus = typeof index.status === "string" ? index.status : "unknown";
        const entryCount = typeof index.entry_count === "number" ? index.entry_count : 0;
        parts.push(`**Index:** ${indexStatus} (${entryCount} entries)`);
      }

      // Metrics
      const metrics = response.metrics;
      if (metrics && typeof metrics === "object") {
        const totalQueries = typeof metrics.total_queries === "number" ? metrics.total_queries : 0;
        const p50 = typeof metrics.p50_latency_ms === "number" ? metrics.p50_latency_ms : 0;
        const p95 = typeof metrics.p95_latency_ms === "number" ? metrics.p95_latency_ms : 0;
        parts.push(
          `**Metrics:** ${totalQueries} queries, p50=${p50.toFixed(0)}ms, p95=${p95.toFixed(0)}ms`,
        );
      }

      // Warnings
      const warnings = response.warnings;
      if (Array.isArray(warnings) && warnings.length > 0) {
        parts.push(`\n**Warnings:** ${warnings.join(", ")}`);
      }

      // Suggestions
      const suggestions = response.suggestions;
      if (Array.isArray(suggestions) && suggestions.length > 0) {
        parts.push("\n**Suggestions:**");
        for (const s of suggestions) {
          const label = typeof s.label === "string" ? s.label : "unknown";
          const message = typeof s.message === "string" ? s.message : "";
          parts.push(`- ${label}: ${message}`);
        }
      }

      if (parts.length === 0) {
        return JSON.stringify(response, null, 2);
      }

      return parts.join("\n");
    },
  };

  return {
    aft_semantic_doctor: doctorTool,
  };
}
