/**
 * aft_model_cache_* tools — manage locally cached model2vec models.
 */

import type { ExtensionAPI, Theme } from "@earendil-works/pi-coding-agent";
import { type Static, Type } from "typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";
import {
  asBoolean,
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

const RepoIdParams = Type.Object({
  repo_id: Type.String({
    description: 'HuggingFace repo ID, e.g. "minishlab/potion-code-16M".',
    minLength: 1,
  }),
});

function formatSizeBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const mb = bytes / 1024 / 1024;
  return `${mb.toFixed(2)} MB`;
}

function buildModelListSections(payload: unknown): string[] {
  const response = asRecord(payload);
  if (!response) return ["No model cache data."];
  const models = asRecords(response.models);
  if (models.length === 0) {
    return ["No cached model2vec models."];
  }
  const lines = models.map((model) => {
    const repoId = asString(model.repo_id) ?? "unknown";
    const path = asString(model.path) ?? "unknown";
    const sizeBytes = asNumber(model.size_bytes) ?? 0;
    return `- ${repoId} (${formatSizeBytes(sizeBytes)}) at ${path}`;
  });
  return ["Cached model2vec models:", ...lines];
}

function buildModelInfoSections(payload: unknown): string[] {
  const response = asRecord(payload);
  if (!response) return ["No model info data."];
  if (response.found === false) {
    return ["Model not found in cache."];
  }
  const repoId = asString(response.repo_id) ?? "unknown";
  const path = asString(response.path) ?? "unknown";
  const downloadedAt = asNumber(response.downloaded_at) ?? 0;
  const sizeBytes = asNumber(response.size_bytes) ?? 0;
  return [
    `Model: ${repoId}`,
    `Path: ${path}`,
    `Downloaded at: ${new Date(downloadedAt * 1000).toISOString()}`,
    `Size: ${formatSizeBytes(sizeBytes)}`,
  ];
}

function buildModelCheckUpdateSections(payload: unknown): string[] {
  const response = asRecord(payload);
  if (!response) return ["No update data."];
  const updateAvailable = asBoolean(response.update_available) ?? false;
  if (updateAvailable) {
    const message = asString(response.message) ?? "Update may be available.";
    return [`Update available: ${message}`];
  }
  return ["No update suggestion. Model is recent."];
}

function renderModelCacheCall(
  name: string,
  args: Static<typeof RepoIdParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  return renderToolCall(name, theme.fg("toolOutput", args.repo_id), theme, context);
}

export function registerModelCacheListTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_model_cache_list",
    label: "model_cache_list",
    description:
      "List cached model2vec models with their paths and sizes. Use to inspect which local semantic-search models are available.",
    parameters: Type.Object({}),
    async execute(_toolCallId: string, _params: Record<string, never>, _signal, _onUpdate, extCtx) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const response = await callBridge(bridge, "model_cache_list", {}, extCtx);
      return textResult(buildModelListSections(response).join("\n"), response);
    },
    renderCall(_args, theme, context) {
      return renderToolCall("model_cache_list", theme.fg("toolOutput", "list"), theme, context);
    },
    renderResult(result, _options, theme, context) {
      if (context.isError)
        return renderErrorResult(result, "model_cache_list failed", theme, context);
      return renderSections(buildModelListSections(extractStructuredPayload(result)), context);
    },
  });
}

export function registerModelCacheInfoTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_model_cache_info",
    label: "model_cache_info",
    description:
      "Get information about a cached model2vec model (path, download time, size). Use to verify a specific cached model.",
    parameters: RepoIdParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof RepoIdParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const response = await callBridge(
        bridge,
        "model_cache_info",
        { repo_id: params.repo_id.trim() },
        extCtx,
      );
      return textResult(buildModelInfoSections(response).join("\n"), response);
    },
    renderCall(args, theme, context) {
      return renderModelCacheCall("model_cache_info", args, theme, context);
    },
    renderResult(result, _options, theme, context) {
      if (context.isError)
        return renderErrorResult(result, "model_cache_info failed", theme, context);
      return renderSections(buildModelInfoSections(extractStructuredPayload(result)), context);
    },
  });
}

export function registerModelCacheRemoveTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_model_cache_remove",
    label: "model_cache_remove",
    description:
      "Remove a cached model2vec model from disk. Use to free space or force a re-download of an updated model.",
    parameters: RepoIdParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof RepoIdParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const response = await callBridge(
        bridge,
        "model_cache_remove",
        { repo_id: params.repo_id.trim() },
        extCtx,
      );
      const text =
        response.success === false
          ? "Failed to remove model cache."
          : "Model cache removed successfully.";
      return textResult(text, response);
    },
    renderCall(args, theme, context) {
      return renderModelCacheCall("model_cache_remove", args, theme, context);
    },
    renderResult(result, _options, theme, context) {
      if (context.isError)
        return renderErrorResult(result, "model_cache_remove failed", theme, context);
      return renderSections([theme.fg("success", "Model cache removed.")], context);
    },
  });
}

export function registerModelCacheCheckUpdateTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_model_cache_check_update",
    label: "model_cache_check_update",
    description:
      "Check whether a cached model2vec model might have an update available. This is a heuristic based on cache age; it does not contact the network.",
    parameters: RepoIdParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof RepoIdParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const response = await callBridge(
        bridge,
        "model_cache_check_update",
        { repo_id: params.repo_id.trim() },
        extCtx,
      );
      return textResult(buildModelCheckUpdateSections(response).join("\n"), response);
    },
    renderCall(args, theme, context) {
      return renderModelCacheCall("model_cache_check_update", args, theme, context);
    },
    renderResult(result, _options, theme, context) {
      if (context.isError)
        return renderErrorResult(result, "model_cache_check_update failed", theme, context);
      return renderSections(
        buildModelCheckUpdateSections(extractStructuredPayload(result)),
        context,
      );
    },
  });
}

export function registerModelCacheTools(pi: ExtensionAPI, ctx: PluginContext): void {
  registerModelCacheListTool(pi, ctx);
  registerModelCacheInfoTool(pi, ctx);
  registerModelCacheRemoveTool(pi, ctx);
  registerModelCacheCheckUpdateTool(pi, ctx);
}
