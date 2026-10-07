import { expect, test } from "bun:test";
import { SubcTransportPool } from "../subc-transport.js";
import { observeFreshSessionStart } from "../transport.js";
import { TEST_PROJECT_ROOT } from "./subc-test-roots.js";

test("only host new and fork events establish an observed session lifetime", async () => {
  for (const reason of ["new", "fork", "startup", "reload", "resume", undefined]) {
    const pool = new SubcTransportPool({
      connectionFile: "/fake",
      harness: "pi",
      connect: async () => ({
        routeOpen: async () => ({ channel: 1, epoch: 1 }),
        request: async () => ({ structuredContent: { success: true, text: "read" } }),
        subscribe: () => {
          throw new Error("no background handler is registered");
        },
        closeRouteChannel: async () => undefined,
        close: () => undefined,
      }),
    });
    try {
      observeFreshSessionStart(pool, TEST_PROJECT_ROOT, "session", reason);
      await pool.toolCall(TEST_PROJECT_ROOT, { sessionID: "session" }, "read");
      await pool.reapIdleSessions(Date.now() + 16 * 60_000);
      expect(pool.__retainedSessionCountsForTests().sessions).toBe(
        reason === "new" || reason === "fork" ? 0 : 1,
      );
    } finally {
      await pool.shutdown();
    }
  }
});

test("unused observed session starts remain bounded", async () => {
  const pool = new SubcTransportPool({ connectionFile: "/fake", harness: "pi" });
  try {
    for (let i = 0; i < 1000; i++)
      observeFreshSessionStart(pool, TEST_PROJECT_ROOT, `unused-${i}`, "new");
    expect(pool.__pendingSessionStartsForTests()).toBe(256);
  } finally {
    await pool.shutdown();
  }
});
