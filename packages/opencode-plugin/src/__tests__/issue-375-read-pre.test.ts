/// <reference path="../bun-test.d.ts" />
import { expect, test } from "bun:test";
import type { ToolContext } from "@opencode-ai/plugin";
import { createReadTool } from "../tools/hoisted.js";
import type { PluginContext } from "../types.js";

async function readTextWithHostProbe(filePath = "README.md") {
  const events: string[] = [];
  const root = process.cwd();
  const ctx = {
    config: {},
    storageDir: root,
    client: {
      session: {
        get: async () => ({ data: { directory: root } }),
        messages: async () => {
          events.push("session.messages");
          return { data: [{ info: { model: { providerID: "probe", modelID: "text" } } }] };
        },
      },
      provider: {
        list: async () => {
          events.push("provider.list");
          return { data: { all: [{ id: "probe", models: { text: { attachment: false } } }] } };
        },
      },
    },
    pool: {
      getBridge: () => ({
        toolCall: async () => {
          events.push("bridge.read");
          return { success: true, text: "1: plain text\n" };
        },
      }),
    },
  } as unknown as PluginContext;
  const context: ToolContext = {
    sessionID: "issue-375-read-pre",
    messageID: "message",
    agent: "test",
    directory: root,
    worktree: root,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: async () => {
      events.push("permission.read");
    },
  };
  const result = await createReadTool(ctx).execute({ filePath }, context);
  return { events, result };
}

test("text read does not fetch host history or model catalog", async () => {
  const { events, result } = await readTextWithHostProbe();
  expect(result).toMatchObject({ output: "1: plain text\n" });
  expect(events).toEqual(["permission.read", "bridge.read"]);
});

test("directory read does not fetch host history or model catalog", async () => {
  const { events } = await readTextWithHostProbe(".");
  expect(events).toEqual(["permission.read", "bridge.read"]);
});
