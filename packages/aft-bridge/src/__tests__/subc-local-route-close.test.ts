/**
 * The plugin closing one of its own routes while a call is in flight on it:
 * pool shutdown (which is also what host-process exit runs), session close,
 * project-root close, and a sibling call discarding the session's shared route.
 *
 * Exercised through a REAL `SubcClient` over a real TCP socket against a small
 * in-process fake subc daemon. The daemon records every data-plane request it
 * receives and never answers it, so a call is provably in flight (its request
 * reached the daemon) when the close happens. A test double that resolved or
 * rejected requests by itself could not show that, and would hide whether the
 * real client's `closeRoute` rejection is what the caller sees.
 */

import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  buildFlags,
  buildFrame,
  computeProof,
  decodeHeader,
  encodeFrame,
  FrameType,
  HEADER_LEN,
  Priority,
  SERVER_PROOF_DOMAIN,
  SubcError,
} from "@cortexkit/subc-client";
import {
  adaptToolError,
  BASH_TRANSPORT_DISPOSITION,
  classifyBashHostFallbackError,
  SUBC_MODULE_RESTART_DISPOSITION,
  SUBC_ROUTE_CLOSED_MID_CALL_DISPOSITION,
} from "../error-contract.js";
import { asCanonicalRootPath, LifecycleRegistry } from "../lifecycle-registry.js";
import {
  isSubcRouteTornDownError,
  SubcRouteClosedMidCallError,
  SubcTransportPool,
  SubcTransportShuttingDownError,
} from "../subc-transport.js";
import { TEST_INFLIGHT_ROOT, TEST_PROJECT_ROOT } from "./subc-test-roots.js";

interface FakeDaemon {
  connectionFile: string;
  /** Data-plane request bodies the daemon received, in arrival order. */
  readonly requests: Array<{ name?: string }>;
  /** While true, route.open requests are held instead of answered. */
  holdRouteOpens: boolean;
  /** Route opens received while held. */
  readonly heldRouteOpens: number;
  /** Answer every held route.open, accepting it. */
  releaseRouteOpens(): void;
  /** Data-plane requests are answered with this body; null holds them forever. */
  reply: unknown;
  close(): Promise<void>;
}

async function startFakeDaemon(): Promise<FakeDaemon> {
  const key = new Uint8Array(32).fill(7);
  const daemonId = new Uint8Array(16).fill(9);
  const sockets = new Set<Socket>();
  const requests: Array<{ name?: string }> = [];
  const heldOpens: Array<() => void> = [];
  let nextChannel = 1;
  const control = {
    holdRouteOpens: false,
    reply: null as unknown,
  };

  const server: Server = createServer((socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.on("error", () => undefined);
    let buffered = Buffer.alloc(0);
    // Handshake messages are 4-byte little-endian length + JSON; after the
    // client's auth message the stream switches to envelope frames.
    let phase: "hello" | "auth" | "frames" = "hello";

    const writeAuthMessage = (value: unknown): void => {
      const json = Buffer.from(JSON.stringify(value), "utf8");
      const prefix = Buffer.alloc(4);
      prefix.writeUInt32LE(json.length, 0);
      socket.write(Buffer.concat([prefix, json]));
    };
    const send = (
      ty: FrameType,
      channel: number,
      epoch: number,
      corr: bigint,
      body: unknown,
    ): void => {
      const bytes =
        body === null ? new Uint8Array(0) : new Uint8Array(Buffer.from(JSON.stringify(body)));
      const frame = buildFrame(
        ty,
        buildFlags(false, Priority.Interactive, false),
        channel,
        epoch,
        corr,
        bytes,
      );
      if (!socket.destroyed) socket.write(encodeFrame(frame));
    };

    socket.on("data", (chunk: Buffer) => {
      buffered = Buffer.concat([buffered, chunk]);
      for (;;) {
        if (phase !== "frames") {
          if (buffered.length < 4) return;
          const len = buffered.readUInt32LE(0);
          if (buffered.length < 4 + len) return;
          const message = JSON.parse(buffered.subarray(4, 4 + len).toString("utf8")) as {
            client_nonce?: number[];
          };
          buffered = buffered.subarray(4 + len);
          if (phase === "hello") {
            const clientNonce = Uint8Array.from(message.client_nonce ?? []);
            const serverNonce = new Uint8Array(32).fill(3);
            writeAuthMessage({
              daemon_id: Array.from(daemonId),
              server_nonce: Array.from(serverNonce),
              daemon_ver: "fake",
              server_proof: Array.from(
                computeProof(key, SERVER_PROOF_DOMAIN, clientNonce, serverNonce, daemonId),
              ),
            });
            // The client's auth proof is not verified: this daemon only exists
            // to script answers for the client under test.
            phase = "auth";
          } else {
            phase = "frames";
          }
          continue;
        }

        if (buffered.length < HEADER_LEN) return;
        const header = decodeHeader(new Uint8Array(buffered.subarray(0, HEADER_LEN)));
        if (buffered.length < HEADER_LEN + header.len) return;
        const body = buffered.subarray(HEADER_LEN, HEADER_LEN + header.len);
        buffered = buffered.subarray(HEADER_LEN + header.len);

        if (header.ty === FrameType.Ping) {
          send(FrameType.Pong, header.channel, header.epoch, header.corr, null);
          continue;
        }
        if (header.ty !== FrameType.Request) continue;

        if (header.channel === 0) {
          const op = (JSON.parse(body.toString("utf8")) as { op?: string }).op;
          if (op !== "route.open") {
            send(FrameType.Error, 0, 0, header.corr, {
              code: "unsupported",
              message: `fake daemon does not implement ${op}`,
            });
            continue;
          }
          const accept = (): void =>
            send(FrameType.Response, 0, 0, header.corr, {
              op: "route.open",
              route_channel: nextChannel++,
              route_epoch: 1,
            });
          if (control.holdRouteOpens) heldOpens.push(accept);
          else accept();
          continue;
        }

        requests.push(JSON.parse(body.toString("utf8")) as { name?: string });
        if (control.reply !== null) {
          send(FrameType.Response, header.channel, header.epoch, header.corr, control.reply);
        }
      }
    });
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (address === null || typeof address === "string") throw new Error("no tcp address");

  const dir = mkdtempSync(join(tmpdir(), "aft-fake-subc-"));
  const connectionFile = join(dir, "connection.json");
  writeFileSync(
    connectionFile,
    JSON.stringify({
      schema: 1,
      wire_version: 2,
      endpoints: [{ host: "127.0.0.1", port: address.port }],
      key: Array.from(key),
      daemon_id: Array.from(daemonId),
      pid: process.pid,
      daemon_ver: "fake",
    }),
  );
  // subc-client refuses a connection file other users could read.
  chmodSync(connectionFile, 0o600);

  return {
    connectionFile,
    requests,
    get holdRouteOpens() {
      return control.holdRouteOpens;
    },
    set holdRouteOpens(value: boolean) {
      control.holdRouteOpens = value;
    },
    get heldRouteOpens() {
      return heldOpens.length;
    },
    releaseRouteOpens() {
      control.holdRouteOpens = false;
      for (const accept of heldOpens.splice(0)) accept();
    },
    get reply() {
      return control.reply;
    },
    set reply(value: unknown) {
      control.reply = value;
    },
    async close() {
      for (const socket of sockets) socket.destroy();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      rmSync(dir, { recursive: true, force: true });
    },
  };
}

const LIVE_REPLY = {
  content: [{ type: "text", text: "live" }],
  isError: false,
  structuredContent: { id: "r", success: true, text: "live" },
};

async function waitFor(condition: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/**
 * The agent-facing outcome of an in-flight bash call cut off by a local close:
 * no host fallback, SUBC_ROUTE_CLOSED_MID_CALL_DISPOSITION (outcome unknown),
 * and neither BASH_TRANSPORT_DISPOSITION (which says to re-run) nor
 * SUBC_MODULE_RESTART_DISPOSITION (which blames a daemon module restart).
 */
function expectUnknownOutcomeForBash(error: unknown, reason: string): void {
  expect(error).toBeInstanceOf(SubcRouteClosedMidCallError);
  const midCall = error as SubcRouteClosedMidCallError;
  expect(midCall.reason).toBe(reason);
  expect(midCall.kind).toBe("outcome_unknown");
  expect(midCall.message).toContain(
    `the command may have run; the route closed mid-call: ${reason}`,
  );
  // The pool keeps subc-client's own rejection as the cause, and recognises it
  // by the client's typed close reason rather than its message text.
  expect(midCall.cause).toBeInstanceOf(SubcError);
  expect((midCall.cause as SubcError).closeReason).toBe("closed_by_caller");

  expect(classifyBashHostFallbackError(error)).toBeUndefined();
  const adapted = adaptToolError("bash", error) as Error;
  expect(adapted.message).toContain(SUBC_ROUTE_CLOSED_MID_CALL_DISPOSITION);
  expect(adapted.message).not.toContain(BASH_TRANSPORT_DISPOSITION);
  expect(adapted.message).not.toContain(SUBC_MODULE_RESTART_DISPOSITION);
}

describe("a route the plugin closes under an in-flight call (real SubcClient, fake daemon)", () => {
  const cleanups: Array<() => Promise<void> | void> = [];
  afterEach(async () => {
    while (cleanups.length > 0) await cleanups.pop()?.();
  });

  async function rig(options: { lifecycle?: LifecycleRegistry } = {}): Promise<{
    pool: SubcTransportPool;
    daemon: FakeDaemon;
  }> {
    const daemon = await startFakeDaemon();
    cleanups.push(() => daemon.close());
    const pool = new SubcTransportPool({
      connectionFile: daemon.connectionFile,
      harness: "opencode",
      consumerIdentity: null,
      handshakeTimeoutMs: 2_000,
      ...(options.lifecycle
        ? {
            lifecycleRegistry: options.lifecycle,
            lifecycleDemandCheck: () => true,
            reapingEnabled: true,
          }
        : {}),
    });
    cleanups.push(() => pool.shutdown());
    return { pool, daemon };
  }

  test("pool shutdown (also run on host exit) reports an in-flight bash call as outcome-unknown", async () => {
    const { pool, daemon } = await rig();
    const call = pool
      .getBridge(TEST_PROJECT_ROOT)
      .send("bash", { command: "touch marker", session_id: "shutdown" });
    const settled = call.catch((error: unknown) => error);
    await waitFor(() => daemon.requests.length === 1, "the bash request to reach the daemon");
    expect(daemon.requests[0]?.name).toBe("bash");

    await pool.shutdown();

    expectUnknownOutcomeForBash(await settled, "transport shutting down");
  });

  test("session close reports an in-flight bash call as outcome-unknown", async () => {
    const { pool, daemon } = await rig();
    const settled = pool
      .getBridge(TEST_PROJECT_ROOT)
      .send("bash", { command: "touch marker", session_id: "closing" })
      .catch((error: unknown) => error);
    await waitFor(() => daemon.requests.length === 1, "the bash request to reach the daemon");

    await pool.closeSession(TEST_PROJECT_ROOT, "closing");

    expectUnknownOutcomeForBash(await settled, "session closed");
  });

  test("project-root close reports an in-flight bash call as outcome-unknown and keeps the reap mark", async () => {
    const registry = new LifecycleRegistry({
      timer: { setInterval: () => 0, clearInterval: () => undefined },
      demandCheck: () => true,
      stat: async () => true,
    });
    const { pool, daemon } = await rig({ lifecycle: registry });
    const root = asCanonicalRootPath(TEST_INFLIGHT_ROOT);
    const bridge = pool.getBridge(root);
    const settled = bridge
      .send("bash", { command: "touch marker", session_id: "reaped" })
      .catch((error: unknown) => error);
    await waitFor(() => daemon.requests.length === 1, "the bash request to reach the daemon");

    const poolId = pool.getConcretePoolId();
    const generation = bridge.getGeneration();
    if (poolId === undefined || generation === undefined) throw new Error("no lifecycle ids");
    await registry.requestProjectRootClose(poolId, root, generation, "explicit");

    const error = await settled;
    expectUnknownOutcomeForBash(error, "project root closed");
    expect((error as { subcTeardownReason?: string }).subcTeardownReason).toBe("root_reaped");
  });

  test("a sibling call discarding the shared route reports the other in-flight call as outcome-unknown", async () => {
    const { pool, daemon } = await rig();
    const bridge = pool.getBridge(TEST_PROJECT_ROOT);
    // Open the session's route first so both calls share it.
    daemon.reply = LIVE_REPLY;
    await bridge.send("read", { session_id: "shared" });
    daemon.reply = null;

    const abort = new AbortController();
    const cancelled = bridge
      .send("read", { session_id: "shared" }, { abortSignal: abort.signal })
      .catch((error: unknown) => error);
    const sibling = bridge
      .send("bash", { command: "touch marker", session_id: "shared" })
      .catch((error: unknown) => error);
    await waitFor(() => daemon.requests.length === 3, "both requests to reach the daemon");

    // Cancelling one call discards the route it shares with the bash call.
    abort.abort();

    expect(((await cancelled) as Error).name).toBe("AbortError");
    expectUnknownOutcomeForBash(await sibling, "route replaced");
  });

  for (const teardown of ["shutdown", "session close"] as const) {
    test(`${teardown} before the request is written stays pre-dispatch, so bash may fall back`, async () => {
      const { pool, daemon } = await rig();
      daemon.holdRouteOpens = true;
      const settled = pool
        .getBridge(TEST_PROJECT_ROOT)
        .send("bash", { command: "touch marker", session_id: "opening" })
        .catch((error: unknown) => error);
      await waitFor(() => daemon.heldRouteOpens === 1, "the route.open to reach the daemon");

      const closing =
        teardown === "shutdown" ? pool.shutdown() : pool.closeSession(TEST_PROJECT_ROOT, "opening");
      daemon.releaseRouteOpens();
      await closing;

      const error = await settled;
      expect(daemon.requests).toHaveLength(0);
      expect(error).not.toBeInstanceOf(SubcRouteClosedMidCallError);
      expect(isSubcRouteTornDownError(error)).toBe(true);
      expect(classifyBashHostFallbackError(error)).toBe("route closed before dispatch");
    });
  }

  test("closing with no request in flight raises nothing, and later calls stay pre-dispatch", async () => {
    const { pool, daemon } = await rig();
    daemon.reply = LIVE_REPLY;
    const bridge = pool.getBridge(TEST_PROJECT_ROOT);
    await bridge.send("read", { session_id: "idle" });

    // A session reopened after an idle close carries on as normal.
    await pool.closeSession(TEST_PROJECT_ROOT, "idle");
    expect(JSON.stringify(await bridge.send("read", { session_id: "idle" }))).toContain("live");

    await pool.shutdown();
    const error = await bridge
      .send("bash", { command: "true", session_id: "idle" })
      .catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(SubcTransportShuttingDownError);
    expect(classifyBashHostFallbackError(error)).toBe("transport down");
    expect(daemon.requests).toHaveLength(2);
  });
});
