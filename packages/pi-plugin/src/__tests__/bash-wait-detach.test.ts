import { afterEach, describe, expect, test } from "bun:test";
import {
  BASH_WAIT_DETACH_MAGIC_KEYWORD,
  interruptBashWaitsForInput,
  shouldDetachBashWaitOnUserMessage,
  stripUserMessageDetachKeyword,
} from "../bash-wait-detach.js";
import { __resetSyncWatchAbortForTests, isSyncWatchAborted } from "../sync-watch-abort.js";

describe("bash wait detach helper (Pi)", () => {
  test("default config detaches on a plain user message", () => {
    expect(shouldDetachBashWaitOnUserMessage({}, "please continue")).toBe(true);
  });

  test("opt-out suppresses plain messages and preserves the keyword escape hatch", () => {
    const config = { bash: { detach_on_user_message: false } };
    const plain = "please continue";
    const message = `please ${BASH_WAIT_DETACH_MAGIC_KEYWORD} continue`;

    expect(shouldDetachBashWaitOnUserMessage(config, plain)).toBe(false);
    expect(shouldDetachBashWaitOnUserMessage(config, message)).toBe(true);
    expect(stripUserMessageDetachKeyword(message)).toBe("please continue");
  });

  test("substitutes an honest message when the token is the only user text", () => {
    expect(stripUserMessageDetachKeyword("  &detach  ")).toBe("(requested background detach)");
  });

  test("strips every token and preserves the rest of a message", () => {
    expect(stripUserMessageDetachKeyword("before &detach middle &detach after")).toBe(
      "before middle after",
    );
  });

  test("recognizes standalone tokens at message boundaries", () => {
    const config = { bash: { detach_on_user_message: false } };

    expect(shouldDetachBashWaitOnUserMessage(config, "&detach, continue")).toBe(true);
    expect(shouldDetachBashWaitOnUserMessage(config, "continue &detach")).toBe(true);
    expect(stripUserMessageDetachKeyword("&detach, continue")).toBe(", continue");
    expect(stripUserMessageDetachKeyword("continue &detach")).toBe("continue ");
  });

  test("does not detach or strip when the keyword is part of an identifier", () => {
    const config = { bash: { detach_on_user_message: false } };
    const messages = [
      "Please document &detachment behavior",
      "keep before&detach unchanged",
      "keep &detach_mode unchanged",
      "keep &detaché unchanged",
    ];

    for (const message of messages) {
      expect(shouldDetachBashWaitOnUserMessage(config, message)).toBe(false);
      expect(stripUserMessageDetachKeyword(message)).toBe(message);
    }
  });
});

// A new input interrupts both blocking waits (a wait:true bash and a sync
// bash_watch) while detach_on_user_message is on, whoever sent it; with the
// setting off, both are protected and only &detach interrupts them.
describe("Pi input interrupts waits by one decision", () => {
  const sessionID = "session-pi";

  afterEach(() => __resetSyncWatchAbortForTests());

  async function deliver(config: Record<string, unknown>, text: string) {
    __resetSyncWatchAbortForTests();
    const sends: string[] = [];
    const bridge = {
      send: async (command: string, params: Record<string, unknown>) => {
        sends.push(`${command}:${String(params.session_id)}`);
        return { success: true };
      },
    };
    const pool = { getActiveBridgeForRoot: () => bridge } as unknown as Parameters<
      typeof interruptBashWaitsForInput
    >[0];
    const delivered = interruptBashWaitsForInput(pool, config, "/repo", sessionID, { text });
    // Let the fire-and-forget detach reach the bridge.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return { detached: sends.includes(`bash_wait_detach:${sessionID}`), delivered };
  }

  test("input detaches and aborts with the default config", async () => {
    const { detached, delivered } = await deliver({}, "subagent finished");

    expect(detached).toBe(true);
    expect(isSyncWatchAborted(sessionID)).toBe(true);
    expect(delivered).toBe("subagent finished");
  });

  test("with detach_on_user_message off input interrupts nothing", async () => {
    const config = { bash: { detach_on_user_message: false } };
    const { detached } = await deliver(config, "please continue");

    expect(detached).toBe(false);
    expect(isSyncWatchAborted(sessionID)).toBe(false);
  });

  test("with detach_on_user_message off &detach still detaches and aborts", async () => {
    const config = { bash: { detach_on_user_message: false } };
    const { detached, delivered } = await deliver(config, "&detach");

    expect(detached).toBe(true);
    expect(isSyncWatchAborted(sessionID)).toBe(true);
    expect(delivered).toBe("(requested background detach)");
  });
});

test("detach preserves indentation and TSV away from the token", () => {
  expect(stripUserMessageDetachKeyword("&detach please\n    print('x')\na\t\tb\n| a  | b |")).toBe(
    " please\n    print('x')\na\t\tb\n| a  | b |",
  );
});
