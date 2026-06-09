import { describe, expect, test } from "bun:test";
import {
  coerceAftStatus,
  formatStatusDialogMessage,
  formatStatusMarkdown,
} from "../shared/status.js";

const baseResponse = Object.freeze({
  version: "0.0.0-test",
  project_root: "/tmp/project",
  features: {
    format_on_edit: false,
    validate_on_edit: "off",
    restrict_to_project_root: false,
    search_index: true,
    semantic_search: true,
  },
  search_index: { status: "ready", files: 4, trigrams: 400 },
  semantic_index: {
    status: "ready",
    entries: 128,
    dimension: 384,
  },
  disk: {
    storage_dir: "/tmp/storage",
    trigram_disk_bytes: 1024,
    semantic_disk_bytes: 2048,
  },
  lsp_servers: 2,
  symbol_cache: { local_entries: 3, warm_entries: 6 },
  storage_dir: "/tmp/storage",
  semantic: {
    backend: "openai_compatible",
    model: "text-embedding-3-small",
    api_key_env: "AFT_SEMANTIC_KEY",
  },
});

describe("coerceAftStatus", () => {
  test("adds backend and model when provided", () => {
    const status = coerceAftStatus(baseResponse as unknown as Record<string, unknown>);

    expect(status.semantic_index.backend).toBe("openai_compatible");
    expect(status.semantic_index.model).toBe("text-embedding-3-small");
    expect(status.semantic_index).not.toHaveProperty("api_key_env");
  });

  test("opencode_status_snapshot_includes_compression_passthrough", () => {
    const status = coerceAftStatus({
      ...baseResponse,
      compression: {
        project: { events: 3, original_tokens: 300, compressed_tokens: 210, savings_tokens: 90 },
        session: { events: 1, original_tokens: 100, compressed_tokens: 70, savings_tokens: 30 },
      },
    } as unknown as Record<string, unknown>);

    expect(status.compression?.project.events).toBe(3);
    expect(status.compression?.session.savings_tokens).toBe(30);
  });

  test("parses status_bar when present", () => {
    const status = coerceAftStatus({
      ...baseResponse,
      status_bar: {
        errors: 7,
        warnings: 13,
        dead_code: 334,
        unused_exports: 222,
        duplicates: 1167,
        todos: 5,
        tier2_stale: true,
      },
    } as unknown as Record<string, unknown>);

    expect(status.status_bar?.errors).toBe(7);
    expect(status.status_bar?.duplicates).toBe(1167);
    expect(status.status_bar?.tier2_stale).toBe(true);
  });

  test("status_bar is undefined when null (Tier-2 not populated)", () => {
    const status = coerceAftStatus({
      ...baseResponse,
      status_bar: null,
    } as unknown as Record<string, unknown>);

    expect(status.status_bar).toBeUndefined();
  });
});

describe("formatStatus* output", () => {
  test("formats backend and model without leaking api key", () => {
    const status = coerceAftStatus(baseResponse as unknown as Record<string, unknown>);
    const dialog = formatStatusDialogMessage(status);
    const markdown = formatStatusMarkdown(status);

    expect(dialog).toContain("backend: openai_compatible");
    expect(dialog).toContain("model: text-embedding-3-small");
    expect(markdown).toContain("**Backend:** openai_compatible");
    expect(markdown).toContain("**Model:** text-embedding-3-small");
    expect(dialog).not.toContain("AFT_SEMANTIC_KEY");
    expect(markdown).not.toContain("AFT_SEMANTIC_KEY");
  });
});
