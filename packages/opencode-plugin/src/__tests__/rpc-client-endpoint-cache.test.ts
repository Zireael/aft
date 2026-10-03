/// <reference path="../bun-test.d.ts" />

// The TUI polls the RPC server (status, warnings). Discovery health-checks
// every live port file in turn before each call; once a server has answered
// warm, later calls go straight to it while it is still the newest live
// server, and fall back to discovery as soon as it is not.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { __resetRpcRedirectsForTest, AftRpcClient } from "../shared/rpc-client.js";
import { __resetRpcNotificationsForTest } from "../shared/rpc-notifications.js";
import { AftRpcServer } from "../shared/rpc-server.js";

const tempRoots = new Set<string>();
const servers = new Set<AftRpcServer>();
const originalFetch = globalThis.fetch;
let healthChecks = 0;
let rpcCalls = 0;

function countFetches(): void {
  healthChecks = 0;
  rpcCalls = 0;
  globalThis.fetch = (async (input: Parameters<typeof fetch>[0], init?: RequestInit) => {
    const url = String(input instanceof Request ? input.url : input);
    if (url.endsWith("/health")) healthChecks += 1;
    if (url.includes("/rpc/")) rpcCalls += 1;
    return originalFetch(input, init);
  }) as typeof fetch;
}

async function startServer(
  storageDir: string,
  directory: string,
  status: () => Record<string, unknown>,
): Promise<AftRpcServer> {
  const server = new AftRpcServer(storageDir, directory);
  server.handle("status", async () => status());
  await server.start();
  servers.add(server);
  return server;
}

afterEach(() => {
  globalThis.fetch = originalFetch;
  for (const server of servers) {
    try {
      server.stop();
    } catch {
      // best-effort
    }
  }
  servers.clear();
  __resetRpcNotificationsForTest();
  __resetRpcRedirectsForTest();
  for (const root of tempRoots) rmSync(root, { recursive: true, force: true });
  tempRoots.clear();
});

describe("AftRpcClient endpoint reuse", () => {
  test("repeated calls health-check the server once, not once per call", async () => {
    const root = mkdtempSync(join(tmpdir(), "aft-rpc-endpoint-"));
    tempRoots.add(root);
    const storageDir = join(root, "storage");
    const projectDir = join(root, "project");
    await startServer(storageDir, projectDir, () => ({ success: true, cache_role: "main" }));

    countFetches();
    const client = new AftRpcClient(storageDir, projectDir);
    for (let i = 0; i < 5; i++) {
      const result = await client.call<Record<string, unknown>>("status", {});
      expect(result.cache_role).toBe("main");
    }

    expect(healthChecks).toBe(1);
    expect(rpcCalls).toBe(5);
  });

  test("a cold answer on the fast path runs discovery without asking the same server twice", async () => {
    const root = mkdtempSync(join(tmpdir(), "aft-rpc-endpoint-"));
    tempRoots.add(root);
    const storageDir = join(root, "storage");
    const projectDir = join(root, "project");
    let warm = true;
    await startServer(storageDir, projectDir, () =>
      warm
        ? { success: true, cache_role: "main" }
        : { success: true, status: "not_initialized", message: "placeholder" },
    );

    const client = new AftRpcClient(storageDir, projectDir);
    expect((await client.call<Record<string, unknown>>("status", {})).cache_role).toBe("main");

    warm = false;
    countFetches();
    const cold = await client.call<Record<string, unknown>>("status", {});
    expect(cold.status).toBe("not_initialized");
    // The cold answer came from the fast path; discovery ran (one health
    // check) but did not ask the same server a second time.
    expect(healthChecks).toBe(1);
    expect(rpcCalls).toBe(1);
  });

  test("a stopped server is not reused", async () => {
    const root = mkdtempSync(join(tmpdir(), "aft-rpc-endpoint-"));
    tempRoots.add(root);
    const storageDir = join(root, "storage");
    const projectDir = join(root, "project");
    const first = await startServer(storageDir, projectDir, () => ({
      success: true,
      cache_role: "first",
    }));
    const client = new AftRpcClient(storageDir, projectDir);
    expect((await client.call<Record<string, unknown>>("status", {})).cache_role).toBe("first");

    first.stop();
    servers.delete(first);
    await startServer(storageDir, projectDir, () => ({ success: true, cache_role: "second" }));

    expect((await client.call<Record<string, unknown>>("status", {})).cache_role).toBe("second");
  });
});
