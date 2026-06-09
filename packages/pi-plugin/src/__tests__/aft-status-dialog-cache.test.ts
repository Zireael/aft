/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { showAftStatusDialog } from "../dialogs/status-dialog.js";
import { makeMockBridge, makePluginContext } from "./tool-test-utils.js";

describe("aft-status dialog cache", () => {
  test("fetches fresh status when cached snapshot belongs to another session", async () => {
    const { bridge, calls } = makeMockBridge((_command, params) => ({
      success: true,
      version: "fresh",
      session: { id: params.session_id },
    }));
    bridge.cacheStatusSnapshot({ version: "stale", session: { id: "old-session" } });

    let renderCount = 0;
    let component: { dispose?: () => void } | undefined;
    try {
      await showAftStatusDialog(
        {} as ExtensionAPI,
        {
          cwd: "/repo",
          sessionManager: { getSessionId: () => "new-session" },
          ui: {
            custom: async (factory: (...args: unknown[]) => unknown) => {
              component = factory(
                { requestRender: () => renderCount++ },
                {},
                {},
                () => undefined,
              ) as { dispose?: () => void };
            },
          },
        } as never,
        makePluginContext(bridge),
      );

      await waitUntil(() => calls.length === 1 && renderCount > 0);
      expect(calls[0].params).toEqual({ session_id: "new-session" });
    } finally {
      component?.dispose?.();
    }
  });
});

async function waitUntil(predicate: () => boolean, timeoutMs = 5_000): Promise<void> {
  const started = Date.now();
  while (!predicate()) {
    if (Date.now() - started > timeoutMs) throw new Error("timed out waiting for condition");
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}
