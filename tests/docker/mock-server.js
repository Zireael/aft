#!/usr/bin/env node
/**
 * aimock-based OpenAI mock server for AFT E2E tests.
 *
 * Simulates a realistic multi-turn agent session:
 *   Turn 1: aft_outline (immediate — tests basic tool execution)
 *   Turn 2: read (after tool result — tests file reading)
 *   Turn 3: grep (delayed — gives trigram index time to build)
 *   Turn 4: aft_search (delayed — gives semantic index time to build)
 *   Turn 5: final text response
 *
 * Each response uses streaming with realistic timing so the session
 * lasts long enough for background threads to complete their work.
 */
const { LLMock } = require("@copilotkit/aimock");
const fs = require("node:fs");

function parsePort(value) {
  const port = Number.parseInt(value || "0", 10);
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    throw new Error(`invalid AIMOCK_PORT: ${value}`);
  }
  return port;
}

const port = parsePort(process.env.AIMOCK_PORT);

// When AFT_E2E_TURN_LOG is set, append one line each time a turn fixture is
// actually served. The harness reads this inline to prove the agent loop
// round-tripped (a tool result was consumed and the next request issued) — a
// hung or no-op session that merely times out cannot produce later turns, so
// this defeats false-green "session completed on timeout" results.
const TURN_LOG = process.env.AFT_E2E_TURN_LOG;
function served(label, response) {
  if (!TURN_LOG) return response;
  return (_req) => {
    try {
      fs.appendFileSync(TURN_LOG, `${label}\n`);
    } catch {
      // best-effort: never fail a served response over turn-log bookkeeping
    }
    return response;
  };
}

async function main() {
  const mock = new LLMock({ port });

  // Turn 1: outline the project (immediate)
  mock.on(
    { sequenceIndex: 0 },
    served("turn-1-outline", {
      toolCalls: [
        {
          name: "aft_outline",
          // v0.18.x renamed the four mutually exclusive aft_outline params
          // (filePath / files / directory / url) into a single `target` that
          // auto-detects file vs directory vs URL via stat() / scheme check.
          // Keeping the old `directory` param here would silently fail with
          // "unsupported param" — the agent would receive an error, never
          // make a follow-up tool call, and the bridge would never spawn.
          arguments: JSON.stringify({ target: "src" }),
        },
      ],
    }),
    { streamingProfile: { ttft: 100, tps: 50 } }
  );

  // Turn 2: read a file
  mock.on(
    { sequenceIndex: 1 },
    served("turn-2-read", {
      toolCalls: [
        {
          name: "read",
          arguments: JSON.stringify({ filePath: "src/main.py" }),
        },
      ],
    }),
    { streamingProfile: { ttft: 500, tps: 40 } }
  );

  // Turn 3: grep for a pattern (by now trigram index should be building/ready)
  mock.on(
    { sequenceIndex: 2 },
    served("turn-3-grep", {
      toolCalls: [
        {
          name: "grep",
          arguments: JSON.stringify({ pattern: "def ", path: "src" }),
        },
      ],
    }),
    { streamingProfile: { ttft: 2000, tps: 30 } }
  );

  // Turn 4: glob for files
  mock.on(
    { sequenceIndex: 3 },
    served("turn-4-glob", {
      toolCalls: [
        {
          name: "glob",
          arguments: JSON.stringify({ pattern: "**/*.py" }),
        },
      ],
    }),
    { streamingProfile: { ttft: 1000, tps: 30 } }
  );

  // Turn 5: semantic search (if available — exercises ONNX/fastembed path)
  mock.on(
    { sequenceIndex: 4 },
    served("turn-5-aft_search", {
      toolCalls: [
        {
          name: "aft_search",
          arguments: JSON.stringify({ query: "greeting function" }),
        },
      ],
    }),
    { streamingProfile: { ttft: 2000, tps: 30 } }
  );

  // Turn 6: edit a file (tests write path)
  mock.on(
    { sequenceIndex: 5 },
    served("turn-6-edit", {
      toolCalls: [
        {
          name: "edit",
          arguments: JSON.stringify({
            filePath: "src/main.py",
            oldString: 'print(greet("world"))',
            newString: 'print(greet("docker"))',
          }),
        },
      ],
    }),
    { streamingProfile: { ttft: 500, tps: 40 } }
  );

  // Turn 7: undo the edit (tests safety/backup path)
  mock.on(
    { sequenceIndex: 6 },
    served("turn-7-undo", {
      toolCalls: [
        {
          name: "aft_safety",
          arguments: JSON.stringify({ op: "undo", filePath: "src/main.py" }),
        },
      ],
    }),
    { streamingProfile: { ttft: 500, tps: 40 } }
  );

  // Turn 8: final response
  mock.on(
    { sequenceIndex: 7 },
    served("turn-8-final", {
      content:
        "I've completed the project exploration. I outlined the structure, read files, searched with grep and semantic search, made an edit, and undid it. All tools are working correctly.",
    }),
    { streamingProfile: { ttft: 500, tps: 50 } }
  );

  // Fallback for any unexpected turns. Keep the marker distinct so the
  // harness can fail loudly instead of treating an un-scripted turn as success.
  mock.onMessage(".*", served("unexpected-fallback", {
    content: "UNEXPECTED_TURN_FALLBACK",
  }));

  await mock.start();
  console.log(`[aimock] listening on ${mock.url}`);
  console.log(`[aimock] configured 8 sequential turns for realistic session`);

  process.on("SIGTERM", async () => {
    await mock.stop();
    process.exit(0);
  });
  process.on("SIGINT", async () => {
    await mock.stop();
    process.exit(0);
  });
}

main().catch((e) => {
  console.error("[aimock] fatal:", e);
  process.exit(1);
});
