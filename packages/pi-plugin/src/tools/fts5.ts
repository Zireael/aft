import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

/**
 * Register FTS5 tools with the Pi coding agent.
 */
export function registerFts5Tool(pi: ExtensionAPI, ctx: PluginContext): void {
  // fts5_search
  pi.registerTool({
    name: "fts5_search",
    label: "search",
    description: [
      "Search the FTS5 index for code symbols, bodies, and file paths.",
      "Returns ranked results with file paths, line numbers, snippets, and match lanes.",
      "Use this when you need fast full-text search over indexed code.",
    ].join("\n"),
    parameters: {
      type: "object" as const,
      properties: {
        query: {
          type: "string",
          description: "Search query string (supports exact, prefix, phrase, terms)",
        },
        top_k: {
          type: "number",
          description: "Maximum number of results (default: 20)",
        },
        scope: {
          type: "string",
          enum: ["all", "symbols", "bodies", "paths"],
          description: "Search scope (default: all)",
        },
      },
      required: ["query"],
    },
    async execute(_toolCallId, params, _signal, _onUpdate, extCtx) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = { query: params.query };
      if (params.top_k !== undefined) req.top_k = params.top_k;
      if (params.scope !== undefined) req.scope = params.scope;
      const response = await callBridge(bridge, "fts5_search", req, extCtx);
      const text = (response.text as string) ?? JSON.stringify(response, null, 2);
      return textResult(text, response);
    },
  });

  // fts5_index
  pi.registerTool({
    name: "fts5_index",
    label: "index",
    description: [
      "Manage the FTS5 index: check status, build/update, rebuild, or prune stale files.",
      "Use action='status' to check index health.",
      "Use action='update' to incrementally index changed files.",
      "Use action='rebuild' to clear and reindex everything.",
      "Use action='prune' to remove files no longer on disk.",
    ].join("\n"),
    parameters: {
      type: "object" as const,
      properties: {
        action: {
          type: "string",
          enum: ["status", "update", "rebuild", "prune"],
          description: "Action to perform (default: update)",
        },
      },
    },
    async execute(_toolCallId, params, _signal, _onUpdate, extCtx) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = {};
      if (params.action !== undefined) req.action = params.action;
      const response = await callBridge(bridge, "fts5_index", req, extCtx);
      const text = (response.text as string) ?? JSON.stringify(response, null, 2);
      return textResult(text, response);
    },
  });

  // fts5_find_symbol
  pi.registerTool({
    name: "fts5_find_symbol",
    label: "find_symbol",
    description: [
      "Find a symbol by exact or prefix name in the FTS5 index.",
      "Returns file path, symbol name, kind, line range, snippet, and match lane.",
      "Use mode='exact' for exact name match (SQL first, then FTS fallback).",
      "Use mode='prefix' for prefix matching (default).",
    ].join("\n"),
    parameters: {
      type: "object" as const,
      properties: {
        name: {
          type: "string",
          description: "Symbol name to find (exact or prefix match)",
        },
        mode: {
          type: "string",
          enum: ["exact", "prefix"],
          description: "Match mode (default: prefix)",
        },
        top_k: {
          type: "number",
          description: "Maximum number of results (default: 20)",
        },
      },
      required: ["name"],
    },
    async execute(_toolCallId, params, _signal, _onUpdate, extCtx) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = { name: params.name };
      if (params.mode !== undefined) req.mode = params.mode;
      if (params.top_k !== undefined) req.top_k = params.top_k;
      const response = await callBridge(bridge, "fts5_find_symbol", req, extCtx);
      const text = (response.text as string) ?? JSON.stringify(response, null, 2);
      return textResult(text, response);
    },
  });

  // fts5_read_symbol
  pi.registerTool({
    name: "fts5_read_symbol",
    label: "read_symbol",
    description: [
      "Read canonical source for a symbol from the FTS5 index.",
      "Accepts symbol_id (from find/search results) or name (exact match).",
      "Returns line-numbered source with metadata.",
      "If name matches multiple symbols, returns candidates for disambiguation.",
    ].join("\n"),
    parameters: {
      type: "object" as const,
      properties: {
        symbol_id: {
          type: "number",
          description: "Symbol ID from a find/search result",
        },
        name: {
          type: "string",
          description: "Exact symbol name to read",
        },
        file: {
          type: "string",
          description: "File path to disambiguate when name matches multiple symbols",
        },
        context_lines: {
          type: "number",
          description: "Number of context lines around the symbol (default: 0)",
        },
      },
    },
    async execute(_toolCallId, params, _signal, _onUpdate, extCtx) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = {};
      if (params.symbol_id !== undefined) req.symbol_id = params.symbol_id;
      if (params.name !== undefined) req.name = params.name;
      if (params.file !== undefined) req.file = params.file;
      if (params.context_lines !== undefined) req.context_lines = params.context_lines;
      const response = await callBridge(bridge, "fts5_read_symbol", req, extCtx);
      const text = (response.text as string) ?? JSON.stringify(response, null, 2);
      return textResult(text, response);
    },
  });

  // fts5_doctor
  pi.registerTool({
    name: "fts5_doctor",
    label: "doctor",
    description: [
      "Diagnose FTS5 index health and configuration.",
      "Reports compiled status, FTS5 availability, runtime config, index stats, and warnings.",
    ].join("\n"),
    parameters: {
      type: "object" as const,
      properties: {},
    },
    async execute(_toolCallId, _params, _signal, _onUpdate, extCtx) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const response = await callBridge(bridge, "fts5_doctor", {}, extCtx);
      const text = (response.text as string) ?? JSON.stringify(response, null, 2);
      return textResult(text, response);
    },
  });
}
