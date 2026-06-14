import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";

const z = tool.schema;

type ToolArg = ToolDefinition["args"][string];

function arg(schema: unknown): ToolArg {
  return schema as ToolArg;
}

export function fts5Tools(ctx: PluginContext): Record<string, ToolDefinition> {
  const searchTool: ToolDefinition = {
    description: [
      "Search the FTS5 index for code symbols, bodies, and file paths.",
      "Returns ranked results with file paths, line numbers, snippets, and match lanes.",
      "Use this when you need fast full-text search over indexed code.",
    ].join("\n"),
    args: {
      query: arg(
        z
          .string()
          .describe("Search query string (supports exact, prefix, phrase, terms)")
      ),
      top_k: arg(
        z
          .number()
          .optional()
          .describe("Maximum number of results (default: 20)")
      ),
      scope: arg(
        z
          .enum(["all", "symbols", "bodies", "paths"])
          .optional()
          .describe("Search scope (default: all)")
      ),
    },
    execute: async (args) => {
      const resp = await callBridge(ctx, "fts5_search", {
        query: args.query as string,
        top_k: args.top_k as number | undefined,
        scope: args.scope as string | undefined,
      });
      return resp;
    },
  };

  const indexTool: ToolDefinition = {
    description: [
      "Manage the FTS5 index: check status, build/update, rebuild, or prune stale files.",
      "Use action='status' to check index health.",
      "Use action='update' to incrementally index changed files.",
      "Use action='rebuild' to clear and reindex everything.",
      "Use action='prune' to remove files no longer on disk.",
    ].join("\n"),
    args: {
      action: arg(
        z
          .enum(["status", "update", "rebuild", "prune"])
          .optional()
          .describe("Action to perform (default: update)")
      ),
    },
    execute: async (args) => {
      const resp = await callBridge(ctx, "fts5_index", {
        action: args.action as string | undefined,
      });
      return resp;
    },
  };

  const findSymbolTool: ToolDefinition = {
    description: [
      "Find a symbol by exact or prefix name in the FTS5 index.",
      "Returns file path, symbol name, kind, line range, snippet, and match lane.",
      "Use mode='exact' for exact name match (SQL first, then FTS fallback).",
      "Use mode='prefix' for prefix matching (default).",
    ].join("\n"),
    args: {
      name: arg(
        z.string().describe("Symbol name to find (exact or prefix match)")
      ),
      mode: arg(
        z
          .enum(["exact", "prefix"])
          .optional()
          .describe("Match mode (default: prefix)")
      ),
      top_k: arg(
        z
          .number()
          .optional()
          .describe("Maximum number of results (default: 20)")
      ),
    },
    execute: async (args) => {
      const resp = await callBridge(ctx, "fts5_find_symbol", {
        name: args.name as string,
        mode: args.mode as string | undefined,
        top_k: args.top_k as number | undefined,
      });
      return resp;
    },
  };

  const readSymbolTool: ToolDefinition = {
    description: [
      "Read canonical source for a symbol from the FTS5 index.",
      "Accepts symbol_id (from find/search results) or name (exact match).",
      "Returns line-numbered source with metadata.",
      "If name matches multiple symbols, returns candidates for disambiguation.",
    ].join("\n"),
    args: {
      symbol_id: arg(
        z
          .number()
          .optional()
          .describe("Symbol ID from a find/search result")
      ),
      name: arg(
        z.string().optional().describe("Exact symbol name to read")
      ),
      file: arg(
        z
          .string()
          .optional()
          .describe("File path to disambiguate when name matches multiple symbols")
      ),
      context_lines: arg(
        z
          .number()
          .optional()
          .describe("Number of context lines around the symbol (default: 0)")
      ),
    },
    execute: async (args) => {
      const resp = await callBridge(ctx, "fts5_read_symbol", {
        symbol_id: args.symbol_id as number | undefined,
        name: args.name as string | undefined,
        file: args.file as string | undefined,
        context_lines: args.context_lines as number | undefined,
      });
      return resp;
    },
  };

  const doctorTool: ToolDefinition = {
    description: [
      "Diagnose FTS5 index health and configuration.",
      "Reports compiled status, FTS5 availability, runtime config, index stats, and warnings.",
    ].join("\n"),
    args: {},
    execute: async () => {
      const resp = await callBridge(ctx, "fts5_doctor", {});
      return resp;
    },
  };

  return {
    fts5_search: searchTool,
    fts5_index: indexTool,
    fts5_find_symbol: findSymbolTool,
    fts5_read_symbol: readSymbolTool,
    fts5_doctor: doctorTool,
  };
}
