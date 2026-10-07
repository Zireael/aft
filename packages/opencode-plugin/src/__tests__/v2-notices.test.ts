import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Effect } from "effect";

import {
  deliverConfigureWarnings,
  sendFeatureAnnouncement,
  sendWarning,
} from "../notifications.js";
import { sendIgnoredMessage } from "../shared/ignored-message.js";

function host() {
  const records: unknown[] = [];
  return {
    records,
    client: {
      location: { directory: "/work/project" },
      session: {
        // An Effect is lazy: merely calling either method delivers nothing.
        prompt: () => Effect.die("informational notices must not prompt the model"),
        synthetic: (input: unknown) => Effect.sync(() => records.push(input)),
      },
    },
  };
}

test("V2 ignored notices run a non-resuming synthetic Effect", async () => {
  const { client, records } = host();
  await sendIgnoredMessage(client, "session-notice", "blocked outside path");
  expect(records).toEqual([
    { sessionID: "session-notice", text: "blocked outside path", resume: false },
  ]);
});

test("V2 notification fallbacks deliver warnings, announcements and configure chat before marking seen", async () => {
  const storage = mkdtempSync(join(tmpdir(), "aft-v2-notices-"));
  const { client, records } = host();
  const marker = join(storage, "opencode", "last_announced_version");
  const writes: unknown[] = [];
  try {
    mkdirSync(join(storage, "opencode"));
    writeFileSync(marker, "0.0.1");
    const options = { client, directory: storage, sessionId: "session-notice" };
    await sendWarning(options, "config parse warning");
    await sendFeatureAnnouncement(options, "9.9.9", ["new feature"], "", storage);
    await deliverConfigureWarnings(
      {
        client,
        sessionId: options.sessionId,
        storageDir: storage,
        pluginVersion: "9.9.9",
        delivery: "chat",
        bridge: {
          send: async (command, params) => {
            if (command === "db_set_state") writes.push(params);
            return { success: true, data: { value: null } };
          },
        },
      },
      [{ kind: "formatter_not_installed", tool: "biome", hint: "Install biome" }],
    );
    expect(records).toHaveLength(3);
    expect(records).toEqual([
      {
        sessionID: "session-notice",
        text: expect.stringContaining("config parse warning"),
        resume: false,
      },
      { sessionID: "session-notice", text: expect.stringContaining("new feature"), resume: false },
      {
        sessionID: "session-notice",
        text: expect.stringContaining("Install biome"),
        resume: false,
      },
    ]);
    expect(readFileSync(marker, "utf8")).toBe("9.9.9");
    expect(writes).toHaveLength(1);
  } finally {
    rmSync(storage, { recursive: true, force: true });
  }
});
