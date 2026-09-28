/**
 * Tests for the cross-extension AFT status observability producer
 * (PRD v2.0 §6, REQ-AFT-001..010).
 *
 * These exercise the production module end to end: real envelopes on a bus,
 * the real `coerceAftStatus` authority path, and the pool contract
 * (`getActiveBridgeForRoot` only — never `getBridge`). Removing the
 * producer's registration, authoritative refresh or session gating breaks
 * these tests.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AftProjectTransport } from "@cortexkit/aft-bridge";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { coerceAftStatus } from "../shared/status.js";
import {
  AFT_STATUS_CHANNEL,
  AFT_STATUS_PROTOCOL_VERSION,
  type AftStatusSnapshotEvent,
  createStatusObservabilityProducer,
  isAftStatusDiscoverEvent,
  registerAftStatusObservability,
  type StatusObservabilityOptions,
} from "../status-observability.js";
import type { PluginContext } from "../types.js";

const PROJECT_ROOT = join(tmpdir(), "aft-observability-test-root");
const HOST_SESSION = "ses_host_1";

interface RecordedEvent {
  channel: string;
  data: Record<string, unknown>;
}

interface FakeBus {
  events: {
    on(channel: string, handler: (data: unknown) => void): () => void;
    emit(channel: string, data: unknown): void;
  };
  emitted: RecordedEvent[];
  emitRaw(channel: string, data: unknown): void;
}

function makeBus(): FakeBus {
  const listeners = new Map<string, Set<(data: unknown) => void>>();
  const emitted: RecordedEvent[] = [];
  return {
    emitted,
    events: {
      on(channel: string, handler: (data: unknown) => void) {
        const set = listeners.get(channel) ?? new Set();
        set.add(handler);
        listeners.set(channel, set);
        return () => set.delete(handler);
      },
      emit(channel: string, data: unknown) {
        emitted.push({ channel, data: data as Record<string, unknown> });
        for (const handler of [...(listeners.get(channel) ?? [])]) handler(data);
      },
    },
    emitRaw(channel: string, data: unknown) {
      for (const handler of [...(listeners.get(channel) ?? [])]) handler(data);
    },
  };
}

interface FakeTransport {
  transport: AftProjectTransport;
  sends: Array<{ command: string; params: Record<string, unknown> }>;
  push(snapshot?: unknown): void;
  attached(): boolean;
}

function statusResponse(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    success: true,
    version: "0.57.2",
    project_root: PROJECT_ROOT,
    canonical_root: PROJECT_ROOT,
    cache_role: "main",
    search_index: { status: "ready", files: 5 },
    semantic_index: { status: "ready", refreshing_count: 0, entries: 10 },
    session: { id: HOST_SESSION, tracked_files: 2, checkpoints: 1 },
    ...overrides,
  };
}

function makeTransport(
  options: { subscribe?: boolean; respond?: () => Record<string, unknown> } = {},
): FakeTransport {
  const sends: Array<{ command: string; params: Record<string, unknown> }> = [];
  let listener: ((snapshot: unknown) => void) | null = null;
  const respond = options.respond ?? ((): Record<string, unknown> => statusResponse());
  const base = {
    async send(command: string, params: Record<string, unknown> = {}) {
      sends.push({ command, params });
      return respond();
    },
    getCwd: () => PROJECT_ROOT,
    getCachedStatus: () => null,
    cacheStatusSnapshot: () => undefined,
    toolCall: async () => ({ text: "", success: true }),
  };
  const transport =
    options.subscribe === false
      ? base
      : {
          ...base,
          subscribeStatus(handler: (snapshot: unknown) => void) {
            listener = handler;
            return () => {
              listener = null;
            };
          },
        };
  return {
    transport: transport as unknown as AftProjectTransport,
    sends,
    push: (snapshot?: unknown) => listener?.(snapshot ?? {}),
    attached: () => listener !== null,
  };
}

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

function snapshotEvents(bus: FakeBus): AftStatusSnapshotEvent[] {
  return bus.emitted
    .filter(
      (entry) =>
        entry.channel === AFT_STATUS_CHANNEL &&
        (entry.data as { type?: unknown }).type === "snapshot",
    )
    .map((entry) => entry.data as unknown as AftStatusSnapshotEvent);
}

function withdrawEvents(bus: FakeBus): Array<Record<string, unknown>> {
  return bus.emitted
    .filter((entry) => (entry.data as { type?: unknown }).type === "withdraw")
    .map((entry) => entry.data);
}

function makeProducer(
  bus: FakeBus,
  transport: AftProjectTransport | null,
  overrides: Partial<StatusObservabilityOptions> = {},
): ReturnType<typeof createStatusObservabilityProducer> {
  return createStatusObservabilityProducer({
    events: bus.events,
    getActiveTransport: () => transport,
    getSessionId: () => HOST_SESSION,
    getCwd: () => PROJECT_ROOT,
    debounceMs: 1,
    ...overrides,
  });
}

describe("AFT status observability producer", () => {
  test("push invalidation → debounced authoritative refresh → envelope with coerced snapshot", async () => {
    const bus = makeBus();
    let fetchCount = 0;
    const fake = makeTransport({
      respond: () => {
        fetchCount += 1;
        // Second authoritative fetch returns changed data so the equality
        // gate lets the new revision through.
        return statusResponse({
          search_index: { status: "ready", files: fetchCount === 1 ? 5 : 7 },
        });
      },
    });
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    expect(fake.sends).toHaveLength(1);
    // Authoritative path passes the host session id (REQ-AFT-002).
    expect(fake.sends[0]).toEqual({ command: "status", params: { session_id: HOST_SESSION } });
    expect(snapshotEvents(bus)).toHaveLength(1);
    // The first publish is exactly what /aft-status renders for this session.
    expect(snapshotEvents(bus)[0]!.payload.snapshot).toEqual(coerceAftStatus(statusResponse()));

    // A pushed, session-stripped raw snapshot is only an invalidation signal:
    // even with different content it must never become the published payload.
    fake.push({ search_index: { status: "pushed-only", files: 999 }, degraded: true });
    await sleep(10);
    expect(snapshotEvents(bus)).toHaveLength(2);
    const published = snapshotEvents(bus)[1]!;
    expect(published.protocolVersion).toBe(AFT_STATUS_PROTOCOL_VERSION);
    expect(published.type).toBe("snapshot");
    expect(published.producerInstanceId).toBe(producer.producerInstanceId);
    expect(published.sessionId).toBe(HOST_SESSION);
    expect(published.revision).toBe(2);
    // Published from the authoritative re-fetch (7), never from the raw push (999).
    expect(published.payload.snapshot.search_index.files).toBe(7);
    expect(published.payload.snapshot.session.id).toBe(HOST_SESSION);
    producer.dispose({ withdraw: false });
  });

  test("burst of invalidations coalesces into one debounced refresh", async () => {
    const bus = makeBus();
    const fake = makeTransport();
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    // One send from activation…
    expect(fake.sends).toHaveLength(1);
    // …then a burst of five invalidations coalesces into exactly ONE more.
    for (let index = 0; index < 5; index += 1) fake.push();
    await sleep(10);
    expect(fake.sends).toHaveLength(2);
    producer.dispose({ withdraw: false });
  });

  test("equality gate: unchanged status does not mint a new revision", async () => {
    const bus = makeBus();
    const fake = makeTransport();
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    expect(snapshotEvents(bus)).toHaveLength(1);
    const sendsAfterFirstPublish = fake.sends.length;
    fake.push();
    await sleep(10);
    expect(snapshotEvents(bus)).toHaveLength(1); // byte-identical → no publish
    expect(fake.sends.length).toBeGreaterThan(sendsAfterFirstPublish); // refresh DID run
    expect(producer.currentRevision()).toBe(1);
    producer.dispose({ withdraw: false });
  });

  test("stale in-flight responses cannot overwrite a newer refresh", async () => {
    const bus = makeBus();
    const pending: Array<{ resolve: (value: Record<string, unknown>) => void }> = [];
    let immediateResponses = 1;
    const fake = makeTransport({
      respond: () => {
        if (immediateResponses > 0) {
          immediateResponses -= 1;
          return statusResponse({ search_index: { status: "ready", files: 5 } });
        }
        return new Promise<Record<string, unknown>>((resolve) => {
          pending.push({ resolve });
        }) as unknown as Record<string, unknown>;
      },
    });
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    // First fetch resolves normally and publishes (files: 5).
    expect(snapshotEvents(bus)).toHaveLength(1);
    expect(snapshotEvents(bus)[0]!.payload.snapshot.search_index.files).toBe(5);

    // Two more refreshes go in-flight and stay pending.
    fake.push();
    await sleep(10);
    expect(pending).toHaveLength(1);
    fake.push();
    await sleep(10);
    expect(pending).toHaveLength(2);

    // Newer request resolves first…
    pending[1]!.resolve(statusResponse({ search_index: { status: "ready", files: 22 } }));
    await sleep(5);
    expect(snapshotEvents(bus)).toHaveLength(2);
    expect(snapshotEvents(bus).at(-1)?.payload.snapshot.search_index.files).toBe(22);
    // …then the older one arrives late and must be dropped.
    pending[0]!.resolve(statusResponse({ search_index: { status: "ready", files: 111 } }));
    await sleep(5);
    expect(snapshotEvents(bus)).toHaveLength(2);
    expect(snapshotEvents(bus).at(-1)?.payload.snapshot.search_index.files).toBe(22);
    producer.dispose({ withdraw: false });
  });

  test("stale-while-revalidate: transient not_initialized never downgrades an initialized snapshot", async () => {
    const bus = makeBus();
    let nextCacheRole = "main";
    const fake = makeTransport({
      respond: () => statusResponse({ cache_role: nextCacheRole }),
    });
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    expect(snapshotEvents(bus)).toHaveLength(1);
    expect(producer.publishedStatus()?.snapshot.cache_role).toBe("main");

    nextCacheRole = "not_initialized";
    fake.push();
    await sleep(10);
    expect(snapshotEvents(bus)).toHaveLength(1); // suppressed
    expect(producer.publishedStatus()?.snapshot.cache_role).toBe("main");
    producer.dispose({ withdraw: false });
  });

  test("first snapshot may be not_initialized (no initialized context to protect)", async () => {
    const bus = makeBus();
    const fake = makeTransport({
      respond: () => statusResponse({ cache_role: "not_initialized" }),
    });
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    expect(snapshotEvents(bus)).toHaveLength(1);
    expect(producer.publishedStatus()?.snapshot.cache_role).toBe("not_initialized");
    producer.dispose({ withdraw: false });
  });

  test("snapshots describing a different project root are never published", async () => {
    const bus = makeBus();
    const fake = makeTransport({
      respond: () =>
        statusResponse({
          project_root: "/some/other/project",
          canonical_root: "/some/other/project",
        }),
    });
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    expect(fake.sends).toHaveLength(1);
    expect(snapshotEvents(bus)).toHaveLength(0);
    expect(producer.publishedStatus()).toBeNull();
    producer.dispose({ withdraw: false });
  });

  test("session change publishes under the new session and drops stale-context responses", async () => {
    const bus = makeBus();
    const fake = makeTransport({
      respond: () =>
        statusResponse({ session: { id: "ses_new", tracked_files: 0, checkpoints: 0 } }),
    });
    const producer = makeProducer(bus, fake.transport);
    producer.activate({ sessionId: "ses_old" });
    await sleep(10);
    expect(snapshotEvents(bus).at(-1)?.sessionId).toBe("ses_old");
    producer.activate({ sessionId: "ses_new" });
    await sleep(10);
    expect(snapshotEvents(bus).at(-1)?.sessionId).toBe("ses_new");
    expect(producer.currentSessionId()).toBe("ses_new");
    producer.dispose({ withdraw: false });
  });
});

describe("discovery (PRD §5.4)", () => {
  test("replays the cached snapshot with the requestId and no extra fetch", async () => {
    const bus = makeBus();
    const fake = makeTransport();
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    const sendsBefore = fake.sends.length;
    bus.emitRaw(AFT_STATUS_CHANNEL, {
      protocolVersion: 1,
      type: "discover",
      requestId: "bridge-7",
    });
    const replay = snapshotEvents(bus).at(-1)!;
    expect(replay.requestId).toBe("bridge-7");
    expect(replay.revision).toBe(1); // same revision — replay, not a new revision
    expect(fake.sends.length).toBe(sendsBefore);
    producer.dispose({ withdraw: false });
  });

  test("with no usable cache performs one authoritative refresh", async () => {
    const bus = makeBus();
    const fake = makeTransport();
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    // Invalidate the cache by switching sessions without a fetch completing.
    producer.dispose({ withdraw: false });
    const freshBus = makeBus();
    const freshProducer = makeProducer(freshBus, fake.transport, {
      getSessionId: () => "ses_other",
    });
    bus.emitRaw(AFT_STATUS_CHANNEL, { protocolVersion: 1, type: "discover", requestId: "b-1" });
    freshBus.emitRaw(AFT_STATUS_CHANNEL, {
      protocolVersion: 1,
      type: "discover",
      requestId: "b-2",
    });
    await sleep(10);
    expect(snapshotEvents(freshBus).at(-1)?.requestId).toBe("b-2");
    freshProducer.dispose({ withdraw: false });
  });

  test("discovery without an active transport never starts one", async () => {
    const bus = makeBus();
    let resolveCount = 0;
    const producer = createStatusObservabilityProducer({
      events: bus.events,
      getActiveTransport: () => {
        resolveCount += 1;
        return null; // lazy-start guarantee: nothing active, nothing spawned
      },
      getSessionId: () => HOST_SESSION,
      getCwd: () => PROJECT_ROOT,
      debounceMs: 1,
    });
    producer.activate();
    bus.emitRaw(AFT_STATUS_CHANNEL, { protocolVersion: 1, type: "discover", requestId: "b-9" });
    await sleep(10);
    expect(resolveCount).toBeGreaterThan(0);
    expect(snapshotEvents(bus)).toHaveLength(0);
    expect(producer.attachedTransport()).toBeNull();
    producer.dispose({ withdraw: false });
  });

  test("ignores malformed discovery events", () => {
    expect(isAftStatusDiscoverEvent({ protocolVersion: 1, type: "discover", requestId: "" })).toBe(
      false,
    );
    expect(isAftStatusDiscoverEvent({ protocolVersion: 1, type: "discover" })).toBe(false);
    expect(isAftStatusDiscoverEvent({ protocolVersion: 2, type: "discover", requestId: "x" })).toBe(
      false,
    );
    expect(
      isAftStatusDiscoverEvent({ protocolVersion: 1, type: "discover", requestId: "ok-1" }),
    ).toBe(true);
  });
});

describe("transport capability matrix (REQ-AFT-003/004/005)", () => {
  test("push-capable transport attaches once and detaches on dispose", async () => {
    const bus = makeBus();
    const fake = makeTransport();
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    expect(fake.attached()).toBe(true);
    const sendsBefore = fake.sends.length;
    producer.dispose();
    expect(fake.attached()).toBe(false);
    fake.push(); // detached — must not trigger anything
    await sleep(10);
    expect(fake.sends.length).toBe(sendsBefore);
  });

  test("subc-style transport (no subscribeStatus) refreshes at boundaries without polling", async () => {
    const bus = makeBus();
    const fake = makeTransport({ subscribe: false });
    const producer = makeProducer(bus, fake.transport);

    let intervalCalls = 0;
    const originalSetInterval = globalThis.setInterval;
    globalThis.setInterval = ((...args: Parameters<typeof setInterval>) => {
      intervalCalls += 1;
      return originalSetInterval(...args);
    }) as typeof setInterval;
    try {
      producer.activate();
      await sleep(10);
      const afterActivation = fake.sends.length;
      expect(afterActivation).toBe(1);

      // Idle: no pushes exist for this transport, and no timer polls for them.
      await sleep(20);
      expect(fake.sends.length).toBe(afterActivation);

      // Required boundaries: session start and turn end (REQ-AFT-005).
      producer.onHostBoundary("session_start", { sessionId: HOST_SESSION });
      await sleep(10);
      producer.onHostBoundary("turn_end");
      await sleep(10);
      expect(fake.sends.length).toBe(afterActivation + 2);
      expect(intervalCalls).toBe(0);
    } finally {
      globalThis.setInterval = originalSetInterval;
    }
    producer.dispose({ withdraw: false });
  });

  test("the producer module never schedules an interval", () => {
    const source = readFileSync(new URL("../status-observability.ts", import.meta.url), "utf8");
    expect(source).not.toMatch(/\bsetInterval\s*\(/);
    expect(source).not.toMatch(/getBridge\s*\(/);
  });
});

describe("shutdown (REQ-AFT-009)", () => {
  test("dispose clears timers, detaches, invalidates in-flight work and withdraws", async () => {
    const bus = makeBus();
    const fake = makeTransport();
    const producer = makeProducer(bus, fake.transport);
    producer.activate();
    await sleep(10);
    expect(snapshotEvents(bus)).toHaveLength(1);

    // Pending debounce timer must not fire after dispose.
    fake.push();
    expect(producer.pendingRefresh()).toBe(true);
    producer.dispose({ withdraw: true });
    await sleep(15);
    expect(snapshotEvents(bus)).toHaveLength(1);

    const withdraws = withdrawEvents(bus);
    expect(withdraws).toHaveLength(1);
    expect(withdraws[0]).toEqual({
      protocolVersion: 1,
      type: "withdraw",
      producerInstanceId: producer.producerInstanceId,
      sessionId: HOST_SESSION,
    });
    expect(producer.publishedStatus()).toBeNull();
    // Discovery after dispose is inert.
    bus.emitRaw(AFT_STATUS_CHANNEL, { protocolVersion: 1, type: "discover", requestId: "late" });
    expect(snapshotEvents(bus)).toHaveLength(1);
  });
});

describe("registration wiring", () => {
  interface CapturedHandlers {
    [event: string]: Array<(_event?: unknown, extCtx?: unknown) => unknown>;
  }

  function makeMockPi(bus: FakeBus): { api: ExtensionAPI; handlers: CapturedHandlers } {
    const handlers: CapturedHandlers = {};
    const api = {
      events: bus.events,
      on(event: string, handler: (_event?: unknown, extCtx?: unknown) => unknown) {
        (handlers[event] ??= []).push(handler);
      },
    } as unknown as ExtensionAPI;
    return { api, handlers };
  }

  function makeMockCtx(transport: AftProjectTransport | null): PluginContext {
    return {
      pool: {
        getActiveBridgeForRoot: () => transport,
        // If the producer ever resolved a spawning path, this would throw.
        getBridge: () => {
          throw new Error("producer must never spawn a bridge");
        },
      },
      config: {},
      storageDir: "/tmp/aft-observability-tests",
    } as unknown as PluginContext;
  }

  test("registers lifecycle handlers, activates, and withdraws on session_shutdown", async () => {
    const bus = makeBus();
    const fake = makeTransport();
    const { api, handlers } = makeMockPi(bus);
    const producer = registerAftStatusObservability(api, makeMockCtx(fake.transport), {
      getCwd: () => PROJECT_ROOT,
      getSessionId: () => HOST_SESSION,
      debounceMs: 1,
      onWarn: () => undefined,
    });
    await sleep(10);
    expect(handlers.session_start?.length).toBe(1);
    expect(handlers.turn_end?.length).toBe(1);
    expect(handlers.session_shutdown?.length).toBe(1);
    expect(producer.currentSessionId()).toBe(HOST_SESSION);
    // Activated at registration: attached + first authoritative publish.
    expect(fake.attached()).toBe(true);
    expect(snapshotEvents(bus)).toHaveLength(1);

    // Discovery over the shared bus is answered through the registered listener.
    bus.emitRaw(AFT_STATUS_CHANNEL, { protocolVersion: 1, type: "discover", requestId: "b-reg" });
    expect(snapshotEvents(bus).at(-1)?.requestId).toBe("b-reg");

    // session_shutdown withdraws (REQ-AFT-009).
    for (const handler of handlers.session_shutdown ?? []) await handler(undefined, undefined);
    expect(withdrawEvents(bus)).toHaveLength(1);
    expect(fake.attached()).toBe(false);
  });

  test("session_start and turn_end drive the producer through host context", async () => {
    const bus = makeBus();
    const fake = makeTransport({ subscribe: false });
    const { api, handlers } = makeMockPi(bus);
    const producer = registerAftStatusObservability(api, makeMockCtx(fake.transport), {
      getCwd: () => PROJECT_ROOT,
      getSessionId: () => undefined,
      debounceMs: 1,
      onWarn: () => undefined,
    });
    const extCtx = {
      cwd: PROJECT_ROOT,
      sessionManager: { getSessionId: () => HOST_SESSION },
    };
    await sleep(10);
    const afterRegistration = fake.sends.length;
    for (const handler of handlers.session_start ?? []) {
      handler(undefined, extCtx);
    }
    await sleep(10);
    expect(producer.currentSessionId()).toBe(HOST_SESSION);
    expect(fake.sends.length).toBeGreaterThan(afterRegistration);
    const afterSessionStart = fake.sends.length;
    for (const handler of handlers.turn_end ?? []) {
      handler(undefined, extCtx);
    }
    await sleep(10);
    expect(fake.sends.length).toBe(afterSessionStart + 1);
    producer.dispose({ withdraw: false });
  });

  test("the extension entry actually registers the producer (registration negative control)", () => {
    const entry = readFileSync(new URL("../index.ts", import.meta.url), "utf8");
    expect(entry).toMatch(/registerAftStatusObservability\(/);
    // And the eager-warmup seam re-activates it once the warm bridge exists.
    expect(entry).toMatch(/statusObservability\?\.activate\(\)/);
  });
});
