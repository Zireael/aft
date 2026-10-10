import { describe, expect, test } from "bun:test";
import { registerStatusCommand } from "../../commands/aft-status.js";
import { createHarness, prepareBinary } from "./helpers.js";

describe("aft-status e2e", () => {
  test("reports cache role and canonical root", async () => {
    const prep = await prepareBinary();
    if (!prep.binaryPath) return;

    const harness = await createHarness(prep, {
      config: { indexes: { trigram: false, semantic: false } },
    });
    try {
      let command: { handler: (args: string, ctx: unknown) => Promise<void> } | undefined;
      registerStatusCommand(
        {
          registerCommand(name: string, def: unknown) {
            if (name === "aft-status") command = def as typeof command;
          },
        } as never,
        {
          pool: harness.pool,
          config: { disabled_tools: [], indexes: { trigram: false, semantic: false } },
          storageDir: harness.path(".aft-storage"),
        } as never,
      );

      const notifications: string[] = [];
      await command?.handler("", {
        cwd: harness.tempDir,
        hasUI: false,
        ui: { notify: (message: string) => notifications.push(message) },
      });

      expect(notifications.join("\n")).toContain("Cache role: main");
      expect(notifications.join("\n")).toContain("Canonical root:");
    } finally {
      await harness.cleanup();
    }
  });
});
