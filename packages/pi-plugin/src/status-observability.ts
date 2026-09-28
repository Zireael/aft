/**
 * Cross-extension AFT status observability producer (PRD v2.0 §6).
 *
 * Publishes authoritative, session-scoped AFT status snapshots to peer
 * Pi/OMP extensions over the shared `ExtensionAPI.events` bus on the
 * `cortexkit:aft:status` channel. The AFT PR ends at this generic channel —
 * no Atelier, bridge-package or presentation imports (REQ-AFT-010).
 *
 * Design rules encoded here:
 * - The published payload always comes from the authoritative
 *   `bridge.send("status", { session_id }) → coerceAftStatus` path
 *   (REQ-AFT-002). Pushed Rust `status_changed` frames are session-stripped
 *   and are treated purely as invalidation signals (REQ-AFT-003).
 * - Transports exposing `subscribeStatus` refresh through a ~200 ms
 *   debounced authoritative fetch; transports without it (subc) refresh only
 *   at host lifecycle/discovery boundaries — never on an interval
 *   (REQ-AFT-004/005).
 * - Only `getActiveBridgeForRoot(cwd)` is ever resolved: activation and
 *   discovery never spawn an otherwise inactive AFT transport (REQ-AFT-006).
 * - Session/project correctness: responses are dropped when the context they
 *   were requested for has since changed, and transient `not_initialized`
 *   downgrades never replace an initialized snapshot for the same live
 *   context (REQ-AFT-007), mirroring the OpenCode sidebar guard.
 * - Equal snapshots do not mint a new revision (REQ-AFT-008).
 */

import { randomUUID } from "node:crypto";
import { type AftProjectTransport, canonicalizeProjectRoot } from "@cortexkit/aft-bridge";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { getActiveSessionId } from "./bg-notifications.js";
import { log, warn } from "./logger.js";
import { type AftStatusSnapshot, coerceAftStatus } from "./shared/status.js";
import { resolveSessionId } from "./tools/_shared.js";
import type { PluginContext } from "./types.js";

/** Producer → consumer channel (PRD §5). */
export const AFT_STATUS_CHANNEL = "cortexkit:aft:status";
export const AFT_STATUS_PROTOCOL_VERSION = 1 as const;
/** Debounce window for push-driven authoritative refreshes (REQ-AFT-003). */
export const DEFAULT_STATUS_DEBOUNCE_MS = 200;

const MAX_SESSION_ID_CHARS = 256;
const MAX_REQUEST_ID_CHARS = 256;
/** Cap on coalesced discovery request ids awaiting the next publish. */
const MAX_PENDING_REQUEST_IDS = 8;

function hasControlChars(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return true;
  }
  return false;
}

/** Published payload — the authoritative snapshot and nothing else (REQ-AFT-002). */
export interface AftObservabilityPayload {
  snapshot: AftStatusSnapshot;
}

export interface AftStatusDiscoverEvent {
  protocolVersion: 1;
  type: "discover";
  requestId: string;
  sessionId?: string;
}

export interface AftStatusSnapshotEvent {
  protocolVersion: 1;
  type: "snapshot";
  producerInstanceId: string;
  sessionId: string;
  revision: number;
  requestId?: string;
  payload: AftObservabilityPayload;
}

export interface AftStatusWithdrawEvent {
  protocolVersion: 1;
  type: "withdraw";
  producerInstanceId: string;
  sessionId?: string;
}

export type AftStatusEvent =
  | AftStatusDiscoverEvent
  | AftStatusSnapshotEvent
  | AftStatusWithdrawEvent;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isBoundedIdentifier(value: unknown, maxChars: number): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= maxChars &&
    !hasControlChars(value)
  );
}

/** Structural guard for bridge → producer discovery (PRD §5.4). */
export function isAftStatusDiscoverEvent(value: unknown): value is AftStatusDiscoverEvent {
  return (
    isRecord(value) &&
    value.protocolVersion === AFT_STATUS_PROTOCOL_VERSION &&
    value.type === "discover" &&
    isBoundedIdentifier(value.requestId, MAX_REQUEST_ID_CHARS) &&
    (value.sessionId === undefined || isBoundedIdentifier(value.sessionId, MAX_SESSION_ID_CHARS))
  );
}

/** Shared event-bus surface of `ExtensionAPI.events`. */
export interface StatusEventBus {
  on(channel: string, handler: (data: unknown) => void): () => void;
  emit(channel: string, data: unknown): void;
}

export interface StatusObservabilityOptions {
  /** Shared host event bus (`pi.events`). */
  events: StatusEventBus;
  /**
   * Resolve ONLY an already-active transport for `cwd` (pool
   * `getActiveBridgeForRoot`). Returning `null` must never spawn anything —
   * this is the lazy-start guarantee (REQ-AFT-006).
   */
  getActiveTransport(cwd: string): AftProjectTransport | null;
  /** Current host session id, if known. */
  getSessionId(): string | undefined;
  /** Current project root (cwd) the producer observes. */
  getCwd(): string;
  /** Debounce for push-driven refreshes; defaults to {@link DEFAULT_STATUS_DEBOUNCE_MS}. */
  debounceMs?: number;
  /** Non-fatal error sink; defaults to `logger.warn`. */
  onWarn?(message: string): void;
}

export interface PublishedAftStatus {
  sessionId: string;
  revision: number;
  snapshot: AftStatusSnapshot;
  serialized: string;
}

export interface StatusObservabilityProducer {
  /** Stable per-extension-instance id (PRD §5.2). */
  readonly producerInstanceId: string;
  /**
   * Re-resolve the active transport and refresh if the session/project/
   * transport identity changed (REQ-AFT-006). Never spawns a transport.
   */
  activate(context?: { cwd?: string; sessionId?: string }): void;
  /** Host lifecycle boundary (session start, turn end, warmup done). */
  onHostBoundary(reason: string, context?: { cwd?: string; sessionId?: string }): void;
  /** Handle a bridge-emitted discovery (replay or one refresh, never a spawn). */
  handleDiscover(requestId: string): void;
  /** Teardown: timers, subscription, in-flight work, optional withdraw (REQ-AFT-009). */
  dispose(options?: { withdraw?: boolean }): void;
  /** Introspection seams (tests / diagnostics). */
  currentSessionId(): string | undefined;
  currentRevision(): number;
  publishedStatus(): PublishedAftStatus | null;
  attachedTransport(): AftProjectTransport | null;
  pendingRefresh(): boolean;
}

interface PublishedEntry extends PublishedAftStatus {
  projectRoot: string | null;
}

/**
 * Cross-project contamination belt, mirroring the OpenCode sidebar's
 * `isSnapshotForContext`: a snapshot describing a different project than the
 * one we asked about is never published (there is no `served_directory`
 * marker in the Pi single-project host, so a mismatch is always a stray).
 * Placeholder snapshots without any root are accepted — they carry no data.
 */
export function isAftSnapshotForContext(snapshot: AftStatusSnapshot, directory: string): boolean {
  const roots = [snapshot.project_root, snapshot.canonical_root].filter(
    (root): root is string => typeof root === "string" && root.length > 0,
  );
  if (roots.length === 0) return true;
  const dir = canonicalizeProjectRoot(directory);
  return roots.some((root) => canonicalizeProjectRoot(root) === dir);
}

/**
 * Stale-while-revalidate guard, mirroring the OpenCode sidebar's
 * `shouldSuppressUninitializedDowngrade`: a transient `not_initialized`
 * snapshot (bridge mid-respawn) must not collapse an already-initialized
 * panel for the same live context (REQ-AFT-007).
 */
export function shouldSuppressUninitializedDowngrade(
  incomingCacheRole: string | undefined,
  haveInitializedForContext: boolean,
): boolean {
  return incomingCacheRole === "not_initialized" && haveInitializedForContext;
}

/**
 * Create the producer. Subscribes to the producer channel for discovery
 * immediately; call {@link StatusObservabilityProducer.activate} once the
 * host wiring is in place, and {@link StatusObservabilityProducer.dispose}
 * on shutdown.
 */
export function createStatusObservabilityProducer(
  options: StatusObservabilityOptions,
): StatusObservabilityProducer {
  const debounceMs = options.debounceMs ?? DEFAULT_STATUS_DEBOUNCE_MS;
  const report = (message: string): void => {
    if (options.onWarn) options.onWarn(message);
    else warn(`[status-observability] ${message}`);
  };

  const producerInstanceId = `aft-${randomUUID()}`;
  let currentCwd = options.getCwd();
  let currentSessionId = options.getSessionId();
  let activeTransport: AftProjectTransport | null = null;
  let detachTransport: (() => void) | null = null;
  let published: PublishedEntry | null = null;
  let revision = 0;
  let generation = 0;
  let disposed = false;
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  const pendingRequestIds: string[] = [];

  // --- envelope emission -----------------------------------------------------

  const emitEvent = (event: AftStatusSnapshotEvent | AftStatusWithdrawEvent): void => {
    try {
      options.events.emit(AFT_STATUS_CHANNEL, event);
    } catch (err) {
      report(`failed to emit status event: ${String(err)}`);
    }
  };

  const emitPublished = (entry: PublishedEntry): void => {
    const requestId = pendingRequestIds.shift();
    if (pendingRequestIds.length > 0) pendingRequestIds.length = 0;
    emitEvent({
      protocolVersion: AFT_STATUS_PROTOCOL_VERSION,
      type: "snapshot",
      producerInstanceId,
      sessionId: entry.sessionId,
      revision: entry.revision,
      ...(requestId !== undefined ? { requestId } : {}),
      payload: { snapshot: entry.snapshot },
    });
  };

  // --- authoritative refresh (REQ-AFT-002/003) ------------------------------

  const applySnapshot = (snapshot: AftStatusSnapshot, requestedRoot: string): void => {
    if (disposed) return;
    if (!isAftSnapshotForContext(snapshot, requestedRoot)) {
      report(`dropping status snapshot for a different project root (${snapshot.project_root})`);
      return;
    }
    const session = currentSessionId ?? snapshot.session.id;
    const incomingRoot =
      typeof snapshot.project_root === "string" && snapshot.project_root.length > 0
        ? snapshot.project_root
        : null;
    const haveInitializedForContext =
      published !== null &&
      published.sessionId === session &&
      published.projectRoot !== null &&
      incomingRoot !== null &&
      canonicalizeProjectRoot(published.projectRoot) === canonicalizeProjectRoot(incomingRoot) &&
      published.snapshot.cache_role !== "not_initialized";
    if (shouldSuppressUninitializedDowngrade(snapshot.cache_role, haveInitializedForContext)) {
      // Transient downgrade: keep the last-good snapshot (REQ-AFT-007).
      if (pendingRequestIds.length > 0 && published) emitPublished(published);
      return;
    }
    // Equality gate (REQ-AFT-008): byte-identical snapshot for the same
    // session does not mint a new revision — but a pending discovery still
    // gets a replay of the current entry.
    const serialized = JSON.stringify(snapshot);
    if (published && published.sessionId === session && published.serialized === serialized) {
      if (pendingRequestIds.length > 0) emitPublished(published);
      return;
    }
    revision += 1;
    published = { sessionId: session, revision, snapshot, serialized, projectRoot: incomingRoot };
    emitPublished(published);
  };

  const runRefresh = async (): Promise<void> => {
    refreshTimer = null;
    if (disposed) return;
    const transport = activeTransport;
    // Never spawn: if no transport is active there is nothing to ask, and a
    // discovery/invalidation must not start one (REQ-AFT-006).
    if (!transport) return;
    const requestedRoot = currentCwd;
    const requestedSession = currentSessionId;
    const myGeneration = ++generation;
    try {
      const response = await transport.send(
        "status",
        requestedSession !== undefined ? { session_id: requestedSession } : {},
      );
      if (disposed || myGeneration !== generation) return; // stale in-flight result
      // Context moved on while the fetch was in flight — drop it rather than
      // publish another project's/session's status.
      if (requestedRoot !== currentCwd || requestedSession !== currentSessionId) return;
      if (!isRecord(response) || response.success === false) return;
      applySnapshot(coerceAftStatus(response), requestedRoot);
    } catch (err) {
      if (!disposed && myGeneration === generation) {
        report(`authoritative status refresh failed: ${String(err)}`);
      }
    }
  };

  const scheduleRefresh = (): void => {
    if (disposed || !activeTransport) return;
    // Trailing debounce: a burst of invalidations collapses into one fetch.
    if (refreshTimer !== null) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      void runRefresh();
    }, debounceMs);
  };

  const invalidate = (): void => {
    scheduleRefresh();
  };

  // --- transport attachment (REQ-AFT-003/004/006) ---------------------------

  const detachCurrentTransport = (): void => {
    if (!detachTransport) return;
    try {
      detachTransport();
    } catch (err) {
      report(`failed to detach status subscription: ${String(err)}`);
    }
    detachTransport = null;
  };

  const attachTransport = (): void => {
    if (!activeTransport || typeof activeTransport.subscribeStatus !== "function") return;
    try {
      // The listener payload is an invalidation signal only; the pushed
      // snapshot is session-stripped and never published directly.
      detachTransport = activeTransport.subscribeStatus(() => invalidate());
    } catch (err) {
      report(`failed to attach status subscription: ${String(err)}`);
      detachTransport = null;
    }
  };

  const activate = (context?: { cwd?: string; sessionId?: string }): void => {
    if (disposed) return;
    let contextChanged = false;
    if (context?.cwd !== undefined && context.cwd !== currentCwd) {
      currentCwd = context.cwd;
      contextChanged = true;
    }
    if (context?.sessionId !== undefined && context.sessionId !== currentSessionId) {
      currentSessionId = context.sessionId;
      contextChanged = true;
    }
    const transport = options.getActiveTransport(currentCwd);
    const transportChanged = transport !== activeTransport;
    if (transportChanged) {
      detachCurrentTransport();
      activeTransport = transport;
      attachTransport();
    }
    if ((contextChanged || transportChanged) && activeTransport) {
      scheduleRefresh();
    }
  };

  // --- discovery (PRD §5.4, REQ-AFT-005/006) --------------------------------

  const handleDiscover = (requestId: string): void => {
    if (disposed || !isBoundedIdentifier(requestId, MAX_REQUEST_ID_CHARS)) return;
    const cacheValidForCurrentSession =
      published !== null &&
      (currentSessionId === undefined || published.sessionId === currentSessionId);
    if (cacheValidForCurrentSession && published) {
      // Replay the latest valid cached snapshot for the current session.
      pendingRequestIds.push(requestId);
      if (pendingRequestIds.length > MAX_PENDING_REQUEST_IDS) pendingRequestIds.splice(0, 1);
      emitPublished(published);
      return;
    }
    // No usable cache: one authoritative refresh — but only if a transport is
    // already active. Discovery never starts an inactive transport.
    activate();
    if (!activeTransport) return;
    pendingRequestIds.push(requestId);
    if (pendingRequestIds.length > MAX_PENDING_REQUEST_IDS) pendingRequestIds.splice(0, 1);
    scheduleRefresh();
  };

  const unsubscribeChannel = options.events.on(AFT_STATUS_CHANNEL, (data) => {
    if (disposed) return;
    try {
      if (isAftStatusDiscoverEvent(data)) handleDiscover(data.requestId);
      // Foreign snapshots/withdraws on our channel are ignored; we only ever
      // answer discoveries.
    } catch (err) {
      report(`failed to handle producer-channel event: ${String(err)}`);
    }
  });

  return {
    producerInstanceId,

    activate,

    onHostBoundary(reason: string, context?: { cwd?: string; sessionId?: string }): void {
      if (disposed) return;
      activate(context);
      // Transports without push capability (subc) refresh at host lifecycle
      // boundaries instead — no interval, no polling (REQ-AFT-005).
      if (activeTransport && typeof activeTransport.subscribeStatus !== "function") {
        log(`status observability boundary refresh (${reason})`);
        scheduleRefresh();
      }
    },

    handleDiscover,

    dispose(disposeOptions?: { withdraw?: boolean }): void {
      if (disposed) return;
      disposed = true;
      generation += 1; // invalidate in-flight fetches
      if (refreshTimer !== null) {
        clearTimeout(refreshTimer);
        refreshTimer = null;
      }
      detachCurrentTransport();
      activeTransport = null;
      try {
        unsubscribeChannel();
      } catch (err) {
        report(`failed to unsubscribe producer channel: ${String(err)}`);
      }
      if (disposeOptions?.withdraw !== false) {
        emitEvent({
          protocolVersion: AFT_STATUS_PROTOCOL_VERSION,
          type: "withdraw",
          producerInstanceId,
          ...(currentSessionId !== undefined ? { sessionId: currentSessionId } : {}),
        });
      }
      published = null;
      pendingRequestIds.length = 0;
    },

    currentSessionId: () => currentSessionId,
    currentRevision: () => revision,
    publishedStatus: () => published,
    attachedTransport: () => activeTransport,
    pendingRefresh: () => refreshTimer !== null,
  };
}

/**
 * Register the producer against the Pi/OMP extension runtime.
 *
 * Wiring (all existing seams, no new timers):
 * - producer channel → bridge discovery replies;
 * - `session_start` / `turn_end` → activation + subc boundary refresh;
 * - `session_shutdown` → withdraw + teardown (REQ-AFT-009).
 */
export function registerAftStatusObservability(
  pi: ExtensionAPI,
  ctx: PluginContext,
  overrides: Partial<StatusObservabilityOptions> = {},
): StatusObservabilityProducer {
  const producer = createStatusObservabilityProducer({
    events: pi.events,
    getActiveTransport: (cwd) => ctx.pool.getActiveBridgeForRoot(cwd),
    getSessionId: () => getActiveSessionId(),
    getCwd: () => process.cwd(),
    ...overrides,
  });

  const contextFrom = (extCtx: unknown): { cwd?: string; sessionId?: string } => {
    const candidate = extCtx as Partial<ExtensionContext> | undefined;
    const cwd = typeof candidate?.cwd === "string" ? candidate.cwd : undefined;
    const sessionId = extCtx ? resolveSessionId(extCtx as ExtensionContext) : undefined;
    return {
      ...(cwd !== undefined ? { cwd } : {}),
      ...(sessionId !== undefined ? { sessionId } : {}),
    };
  };

  (
    pi.on as (
      event: "session_start",
      handler: (_event?: unknown, extCtx?: unknown) => unknown,
    ) => void
  )("session_start", (_event, extCtx) => {
    producer.onHostBoundary("session_start", contextFrom(extCtx));
  });

  (pi.on as (event: "turn_end", handler: (_event?: unknown, extCtx?: unknown) => unknown) => void)(
    "turn_end",
    (_event, extCtx) => {
      producer.onHostBoundary("turn_end", contextFrom(extCtx));
    },
  );

  pi.on("session_shutdown", () => {
    producer.dispose({ withdraw: true });
  });

  producer.activate();
  log("status observability producer registered");
  return producer;
}
