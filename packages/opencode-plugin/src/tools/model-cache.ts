import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";

const z = tool.schema;

type ToolArg = ToolDefinition["args"][string];

function arg(schema: unknown): ToolArg {
  return schema as ToolArg;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function formatModelList(response: Record<string, unknown>): string {
  const models = response.models;
  if (!Array.isArray(models) || models.length === 0) {
    return "No cached model2vec models.";
  }
  const lines = models.map((model) => {
    const record = asRecord(model);
    if (!record) return "- (invalid entry)";
    const repoId = typeof record.repo_id === "string" ? record.repo_id : "unknown";
    const path = typeof record.path === "string" ? record.path : "unknown";
    const sizeBytes = typeof record.size_bytes === "number" ? record.size_bytes : 0;
    const sizeMb = (sizeBytes / 1024 / 1024).toFixed(2);
    return `- ${repoId} (${sizeMb} MB) at ${path}`;
  });
  return `Cached model2vec models:\n${lines.join("\n")}`;
}

function formatInfo(response: Record<string, unknown>): string {
  if (response.found === false) {
    return "Model not found in cache.";
  }
  const record = asRecord(response);
  if (!record) return JSON.stringify(response, null, 2);
  const repoId = typeof record.repo_id === "string" ? record.repo_id : "unknown";
  const path = typeof record.path === "string" ? record.path : "unknown";
  const downloadedAt = typeof record.downloaded_at === "number" ? record.downloaded_at : 0;
  const sizeBytes = typeof record.size_bytes === "number" ? record.size_bytes : 0;
  const sizeMb = (sizeBytes / 1024 / 1024).toFixed(2);
  return `Model: ${repoId}\nPath: ${path}\nDownloaded at: ${new Date(downloadedAt * 1000).toISOString()}\nSize: ${sizeMb} MB`;
}

function formatCheckUpdate(response: Record<string, unknown>): string {
  if (response.update_available === true) {
    const message =
      typeof response.message === "string" ? response.message : "Update may be available.";
    return `Update available: ${message}`;
  }
  return "No update suggestion. Model is recent.";
}

/**
 * aft_model_cache_* tools — manage locally cached model2vec models.
 */
export function modelCacheTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const repoIdArg = arg(
    z.string().min(1).describe('HuggingFace repo ID, e.g. "minishlab/potion-code-16M".'),
  );

  const listTool: ToolDefinition = {
    description:
      "List cached model2vec models with their paths and sizes. Use to inspect which local semantic-search models are available.",
    args: {},
    execute: async (_args, context): Promise<string> => {
      const response = await callBridge(ctx, context, "model_cache_list", {});
      if (response.success === false) {
        const message =
          typeof response.message === "string" && response.message.length > 0
            ? response.message
            : "model_cache_list failed";
        const code =
          typeof response.code === "string" && response.code.length > 0 ? response.code : undefined;
        throw new Error(code ? `model_cache_list: ${code} — ${message}` : message);
      }
      return formatModelList(response as Record<string, unknown>);
    },
  };

  const infoTool: ToolDefinition = {
    description:
      "Get information about a cached model2vec model (path, download time, size). Use to verify a specific cached model.",
    args: {
      repo_id: repoIdArg,
    },
    execute: async (args, context): Promise<string> => {
      if (typeof args.repo_id !== "string" || args.repo_id.trim().length === 0) {
        throw new Error("model_cache_info: repo_id must be a non-empty string");
      }
      const response = await callBridge(ctx, context, "model_cache_info", {
        repo_id: args.repo_id.trim(),
      });
      if (response.success === false) {
        const message =
          typeof response.message === "string" && response.message.length > 0
            ? response.message
            : "model_cache_info failed";
        const code =
          typeof response.code === "string" && response.code.length > 0 ? response.code : undefined;
        throw new Error(code ? `model_cache_info: ${code} — ${message}` : message);
      }
      return formatInfo(response as Record<string, unknown>);
    },
  };

  const removeTool: ToolDefinition = {
    description:
      "Remove a cached model2vec model from disk. Use to free space or force a re-download of an updated model.",
    args: {
      repo_id: repoIdArg,
    },
    execute: async (args, context): Promise<string> => {
      if (typeof args.repo_id !== "string" || args.repo_id.trim().length === 0) {
        throw new Error("model_cache_remove: repo_id must be a non-empty string");
      }
      const response = await callBridge(ctx, context, "model_cache_remove", {
        repo_id: args.repo_id.trim(),
      });
      if (response.success === false) {
        const message =
          typeof response.message === "string" && response.message.length > 0
            ? response.message
            : "model_cache_remove failed";
        const code =
          typeof response.code === "string" && response.code.length > 0 ? response.code : undefined;
        throw new Error(code ? `model_cache_remove: ${code} — ${message}` : message);
      }
      return "Model cache removed successfully.";
    },
  };

  const checkUpdateTool: ToolDefinition = {
    description:
      "Check whether a cached model2vec model might have an update available. This is a heuristic based on cache age; it does not contact the network.",
    args: {
      repo_id: repoIdArg,
    },
    execute: async (args, context): Promise<string> => {
      if (typeof args.repo_id !== "string" || args.repo_id.trim().length === 0) {
        throw new Error("model_cache_check_update: repo_id must be a non-empty string");
      }
      const response = await callBridge(ctx, context, "model_cache_check_update", {
        repo_id: args.repo_id.trim(),
      });
      if (response.success === false) {
        const message =
          typeof response.message === "string" && response.message.length > 0
            ? response.message
            : "model_cache_check_update failed";
        const code =
          typeof response.code === "string" && response.code.length > 0 ? response.code : undefined;
        throw new Error(code ? `model_cache_check_update: ${code} — ${message}` : message);
      }
      return formatCheckUpdate(response as Record<string, unknown>);
    },
  };

  return {
    aft_model_cache_list: listTool,
    aft_model_cache_info: infoTool,
    aft_model_cache_remove: removeTool,
    aft_model_cache_check_update: checkUpdateTool,
  };
}
