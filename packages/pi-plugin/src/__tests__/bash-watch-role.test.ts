import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { BinaryBridge } from "@cortexkit/aft-bridge";
import { LONGEST_TIMER_DELAY_MS, watchClock, watchTimeoutSteer } from "@cortexkit/aft-bridge";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { __resetSyncWatchAbortForTests, signalSyncWatchAbort } from "../sync-watch-abort.js";
import { resolveSessionId } from "../tools/_shared.js";
import { registerBashTool, watchCallerRole } from "../tools/bash.js";
import type { PluginContext } from "../types.js";

// bash_watch words its timeout reply, and picks its default deadline, by the
// caller's role. Pi has no parent-session link, so a headless context
// (`hasUI: false`, the `pi --print` children delegated agents run in) is the
// worker signal; interactive and RPC contexts have a UI and are primary.

interface MockToolDef {
  name: string;
  execute: (
    toolCallId: string,
    params: Record<string, unknown>,
    signal: AbortSignal | undefined,
    onUpdate: ((update: unknown) => void) | undefined,
    ctx: { cwd: string; hasUI?: boolean },
  ) => Promise<unknown>;
}

type ToolResult = {
  content: Array<{ type: string; text: string }>;
  details: Record<string, unknown>;
};

const SUBAGENT_ENV = "MAGIC_CONTEXT_PI_SUBAGENT";
let savedSubagentEnv: string | undefined;

beforeEach(() => {
  savedSubagentEnv = process.env[SUBAGENT_ENV];
  delete process.env[SUBAGENT_ENV];
});

afterEach(() => {
  if (savedSubagentEnv === undefined) delete process.env[SUBAGENT_ENV];
  else process.env[SUBAGENT_ENV] = savedSubagentEnv;
});

function registeredTool(
  name: string,
  send: (
    command: string,
    params: Record<string, unknown>,
    options?: Record<string, unknown>,
  ) => Record<string, unknown>,
  config: Record<string, unknown> = {},
): MockToolDef {
  const bridge = {
    send: async (
      command: string,
      params: Record<string, unknown>,
      options?: Record<string, unknown>,
    ) => send(command, params, options),
  } as unknown as BinaryBridge;
  const ctx = {
    pool: { getBridge: () => bridge } as PluginContext["pool"],
    config: config as PluginContext["config"],
    storageDir: "/tmp/test",
  } satisfies PluginContext;
  const tools = new Map<string, MockToolDef>();
  registerBashTool(
    { registerTool: (tool: MockToolDef) => tools.set(tool.name, tool) } as unknown as ExtensionAPI,
    ctx,
  );
  const tool = tools.get(name);
  if (!tool) throw new Error(`${name} was not registered`);
  return tool;
}

function watchTool(
  send: (command: string) => Record<string, unknown>,
  config: Record<string, unknown> = {},
): MockToolDef {
  return registeredTool("bash_watch", (command) => send(command), config);
}

async function watch(
  tool: MockToolDef,
  params: Record<string, unknown>,
  hasUI: boolean,
  signal?: AbortSignal,
): Promise<ToolResult> {
  return (await tool.execute("call", params, signal, undefined, {
    cwd: process.cwd(),
    hasUI,
  })) as ToolResult;
}

/**
 * Replaces the bash_watch clock with simulated time: each sleep advances the
 * clock by its duration and returns at once, so minutes of waiting run in
 * milliseconds. Call `restore` when done.
 */
function useFakeWatchClock(): { now: () => number; restore: () => void } {
  const real = { now: watchClock.now, sleep: watchClock.sleep };
  let nowMs = 0;
  watchClock.now = () => nowMs;
  watchClock.sleep = async (ms: number) => {
    nowMs += ms;
  };
  return {
    now: () => nowMs,
    restore: () => {
      watchClock.now = real.now;
      watchClock.sleep = real.sleep;
    },
  };
}

describe("Pi bash_watch caller role", () => {
  test("watchCallerRole: headless or pi-magic-context subagent is a worker, UI is primary", () => {
    expect(watchCallerRole({ hasUI: false }, {})).toBe("worker");
    expect(watchCallerRole({ hasUI: true }, {})).toBe("primary");
    expect(watchCallerRole(undefined, {})).toBe("primary");
    expect(watchCallerRole({ hasUI: true }, { [SUBAGENT_ENV]: "1" })).toBe("worker");
  });

  test("timeout gives a headless worker only the worker steer, with no cap", async () => {
    const tool = watchTool(() => ({ success: true, status: "running" }), {
      bash: { watch_sync_max_ms: 90_000 },
    });
    const result = await watch(tool, { task_id: "bash-worker", timeout_ms: 1 }, false);
    const text = result.content[0].text;
    expect(text).toContain("timeout reached without match");
    expect(text).toContain(watchTimeoutSteer("worker", "timeout_ms"));
    expect(text).toContain("a bash_watch without timeout_ms waits until the command finishes");
    expect(text).not.toContain("up to");
    expect(text).not.toContain("90000");
    expect(text).not.toContain("end your turn");
    expect(text).not.toContain("don't poll");
    expect(text).not.toContain("completion reminder");
  });

  test("timeout gives an interactive primary only the primary steer", async () => {
    const tool = watchTool(() => ({ success: true, status: "running" }));
    const result = await watch(tool, { task_id: "bash-primary", timeout_ms: 1 }, true);
    const text = result.content[0].text;
    expect(text).toContain("timeout reached without match");
    expect(text).toContain(watchTimeoutSteer("primary"));
    expect(text).not.toContain("don't report a result");
  });

  test("without timeout_ms a worker's watch has no deadline and a primary's 30000", async () => {
    const effectiveFor = async (hasUI: boolean) => {
      let polls = 0;
      const tool = watchTool(() => {
        polls += 1;
        return polls === 1
          ? { success: true, status: "running" }
          : { success: true, status: "completed", exit_code: 0 };
      });
      const result = await watch(tool, { task_id: `bash-default-${hasUI}` }, hasUI);
      return result.details.effectiveWaitMs;
    };
    expect(await effectiveFor(false)).toBeUndefined();
    expect(await effectiveFor(true)).toBe(30_000);
  });

  // The rest run on simulated time (useFakeWatchClock), so a wait of several
  // minutes finishes in milliseconds.

  test("a worker's watch without timeout_ms returns only when the task exits, past 120 s", async () => {
    __resetSyncWatchAbortForTests();
    const clock = useFakeWatchClock();
    try {
      let polls = 0;
      const tool = watchTool(() => {
        polls += 1;
        return clock.now() < 150_000
          ? { success: true, status: "running" }
          : { success: true, status: "completed", exit_code: 0 };
      });
      const text = (await watch(tool, { task_id: "bash-worker-long" }, false)).content[0].text;
      expect(text).toContain("task exited (completed, exit 0)");
      expect(text).toContain("no limit: waits until the command finishes");
      expect(text).not.toContain("timeout reached");
      expect(Number(/Waited (\d+)ms/.exec(text)?.[1])).toBeGreaterThanOrEqual(150_000);
      // The poll interval backs off, so a long wait stays cheap: at a fixed
      // 100 ms this wait would have taken 1500 status polls.
      expect(polls).toBeLessThan(500);
    } finally {
      clock.restore();
    }
  });

  test("a new message ends a worker's watch that has no deadline", async () => {
    __resetSyncWatchAbortForTests();
    const clock = useFakeWatchClock();
    const sessionId = resolveSessionId({ cwd: process.cwd(), hasUI: false } as never);
    try {
      const tool = watchTool(() => {
        if (clock.now() >= 200_000) signalSyncWatchAbort(sessionId);
        return { success: true, status: "running", mode: "pipes" };
      });
      const text = (await watch(tool, { task_id: "bash-worker-message" }, false)).content[0].text;
      expect(text).toContain("interrupted because you sent a message");
      expect(text).toContain("Call bash_watch again to keep waiting");
      expect(text).not.toContain("completion reminder");
      expect(clock.now()).toBeGreaterThanOrEqual(200_000);
      expect(clock.now()).toBeLessThan(202_000);
    } finally {
      clock.restore();
    }
  });

  test("an aborted tool call ends a worker's watch that has no deadline", async () => {
    __resetSyncWatchAbortForTests();
    const clock = useFakeWatchClock();
    const controller = new AbortController();
    try {
      const tool = watchTool(() => {
        if (clock.now() >= 300_000) controller.abort();
        return { success: true, status: "running" };
      });
      const text = (await watch(tool, { task_id: "bash-worker-abort" }, false, controller.signal))
        .content[0].text;
      expect(text).toContain("the watch was cancelled");
      expect(clock.now()).toBeLessThan(302_000);
    } finally {
      clock.restore();
    }
  });

  test("a worker's explicit timeout_ms above the cap is honoured as given", async () => {
    __resetSyncWatchAbortForTests();
    const clock = useFakeWatchClock();
    try {
      const tool = watchTool(() => ({ success: true, status: "running" }));
      const text = (
        await watch(tool, { task_id: "bash-worker-explicit", timeout_ms: 600_000 }, false)
      ).content[0].text;
      expect(text).toMatch(/Waited 600000ms \(limit 600000ms\); timeout reached without match/);
    } finally {
      clock.restore();
    }
  });

  test("a primary's watch without timeout_ms still ends at 30 s", async () => {
    __resetSyncWatchAbortForTests();
    const clock = useFakeWatchClock();
    try {
      const tool = watchTool(() => ({ success: true, status: "running" }));
      const text = (await watch(tool, { task_id: "bash-primary-default" }, true)).content[0].text;
      expect(text).toMatch(/Waited 30000ms \(limit 30000ms\); timeout reached without match/);
      expect(text).toContain(watchTimeoutSteer("primary"));
    } finally {
      clock.restore();
    }
  });
});

describe("Pi bash wait:true caller role", () => {
  async function waitCall(params: Record<string, unknown>, hasUI: boolean) {
    const calls: Array<[Record<string, unknown>, Record<string, unknown> | undefined]> = [];
    const tool = registeredTool("bash", (_command, sent, options) => {
      calls.push([sent, options]);
      return {
        success: true,
        status: "completed",
        task_id: "task-wait",
        exit_code: 0,
        output: "ok",
      };
    });
    await tool.execute("call", { command: "long-build", ...params }, undefined, undefined, {
      cwd: process.cwd(),
      hasUI,
    });
    return calls[0];
  }

  test("a worker's wait:true without a timeout asks the engine for no default hard kill", async () => {
    const [params, options] = await waitCall({ wait: true }, false);
    expect(params.wait).toBe(true);
    expect(params.timeout).toBeUndefined();
    expect(params.worker_session).toBe(true);
    // The 30-minute default no longer bounds the call, so neither may the
    // transport: it waits as long as a timer can.
    expect(options?.transportTimeoutMs).toBe(LONGEST_TIMER_DELAY_MS);
  });

  test("a worker's wait:true with an explicit timeout keeps it", async () => {
    const [params, options] = await waitCall({ wait: true, timeout: 45_000 }, false);
    expect(params.timeout).toBe(45_000);
    expect(options?.transportTimeoutMs).toBe(45_000 + 10_000);
  });

  test("a primary's wait:true without a timeout keeps the 30-minute budget", async () => {
    const [params, options] = await waitCall({ wait: true }, true);
    expect(params.worker_session).toBe(false);
    expect(options?.transportTimeoutMs).toBe(30 * 60 * 1000 + 10_000);
  });

  test("a worker's background launch does not promise a completion reminder", async () => {
    const tool = registeredTool("bash", () => ({
      success: true,
      status: "running",
      task_id: "bash-bg",
      output:
        "Background task started: bash-bg. A completion reminder will be delivered automatically; don't poll bash_status.",
    }));
    const result = (await tool.execute(
      "call",
      { command: "sleep 30", background: true },
      undefined,
      undefined,
      { cwd: process.cwd(), hasUI: false },
    )) as ToolResult;
    const text = result.content[0].text;
    expect(text).toContain("Background task started: bash-bg.");
    expect(text).toContain("call bash_watch without a timeout");
    expect(text).not.toContain("completion reminder");
  });
});
