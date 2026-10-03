import type {
  AftProjectTransport,
  BashCompletedPayload,
  BashLongRunningPayload,
  BridgeOptions,
} from "@cortexkit/aft-bridge";
import { Effect } from "effect";

import { formatLongRunningReminder, formatSystemReminder } from "../bg-notifications.js";
import { warn } from "../logger.js";
import { executeV2Bash } from "../tools/bash/executor.js";
import type { V2ToolConsumers } from "../tools/definitions/v2.js";
import { type V2SessionAdmission, V2SessionDelivery } from "./session-delivery.js";

export interface V2WakeHostContext {
  readonly session: V2SessionAdmission;
}

/**
 * How one Location delivers bridge notifications for its sessions.
 *
 * Routes are stored process-wide and may be called by a different copy of this
 * module (see `wakeRouteState`), possibly one of a different plugin version
 * with its own `effect` instance. So a route exposes only plain
 * Promise-returning functions built by the copy that owns the session, and the
 * shape must stay backward compatible for as long as the state key is unchanged.
 */
interface WakeRoute {
  /** Deliver one completion to its session and acknowledge it on `bridge` once admitted. */
  completion(completion: BashCompletedPayload, bridge: AftProjectTransport): Promise<void>;
  /** Record a status-only long-running reminder in its session. */
  longRunning(reminder: BashLongRunningPayload): Promise<void>;
}

/** A completion that arrived while no Location had its session registered. */
interface UnroutedCompletion {
  readonly completion: BashCompletedPayload;
  /** The bridge that pushed it, which is where the acknowledgement must go. */
  readonly bridge: AftProjectTransport;
}

interface WakeRouteState {
  /** Session id -> the route of the Location that last registered it. */
  readonly routes: Map<string, WakeRoute>;
  /** Session id -> task id -> completion waiting for its session to register. */
  readonly unrouted: Map<string, Map<string, UnroutedCompletion>>;
  /** Sessions already warned about since they were last routed, to log once per session. */
  readonly warnedUnrouted: Set<string>;
}

/**
 * The standalone bridge pool is one per process (see `location-lifecycle.ts`
 * in aft-bridge) and keeps the notification callbacks of the first Location
 * that created it. OpenCode 2 builds a runtime per Location and can load a
 * separate copy of this module for each (seen with a local-source plugin; an
 * npm plugin gets a second copy when a newly installed version is loaded from
 * its own directory while the old one still runs). A module-level table would then hold only the
 * sessions of the first Location, and completions for every other Location's
 * sessions would be dropped. Keeping the table on `globalThis` under a
 * `Symbol.for` key makes every copy share it.
 */
const WAKE_ROUTE_STATE: unique symbol = Symbol.for(
  "@cortexkit/aft-opencode/wake-routes/v1",
) as never;

type WakeRouteGlobal = typeof globalThis & {
  [WAKE_ROUTE_STATE]?: WakeRouteState;
};

function wakeRouteState(): WakeRouteState {
  const host = globalThis as WakeRouteGlobal;
  return (host[WAKE_ROUTE_STATE] ??= {
    routes: new Map(),
    unrouted: new Map(),
    warnedUnrouted: new Set(),
  });
}

/**
 * Bound on completions held for sessions no Location has registered, so a
 * session that never comes back cannot grow the table without limit. A
 * completion evicted here stays unacknowledged in the bridge.
 */
const MAX_UNROUTED_COMPLETIONS = 256;

function holdUnrouted(completion: BashCompletedPayload, bridge: AftProjectTransport): void {
  const { unrouted } = wakeRouteState();
  let held = 0;
  for (const tasks of unrouted.values()) held += tasks.size;
  const sessionTasks = unrouted.get(completion.session_id);
  if (!sessionTasks?.has(completion.task_id) && held >= MAX_UNROUTED_COMPLETIONS) {
    // Map iteration follows insertion order, so this drops every completion
    // of the session that was first held and has waited longest.
    const oldest = unrouted.keys().next();
    if (!oldest.done) unrouted.delete(oldest.value);
  }
  const tasks = unrouted.get(completion.session_id) ?? new Map<string, UnroutedCompletion>();
  tasks.set(completion.task_id, { completion, bridge });
  unrouted.set(completion.session_id, tasks);
}

function warnUnroutedOnce(sessionID: string, message: string): void {
  const { warnedUnrouted } = wakeRouteState();
  if (warnedUnrouted.has(sessionID)) return;
  warnedUnrouted.add(sessionID);
  warn(message);
}

async function routeCompletion(
  completion: BashCompletedPayload,
  bridge: AftProjectTransport,
): Promise<void> {
  const route = wakeRouteState().routes.get(completion.session_id);
  if (route) return route.completion(completion, bridge);

  // Not acknowledged, so the bridge keeps it pending. It is held here and
  // delivered when a Location registers the session.
  holdUnrouted(completion, bridge);
  warnUnroutedOnce(
    completion.session_id,
    `[bash_completion] no OpenCode Location has session ${completion.session_id} registered; completion of task ${completion.task_id} is held unacknowledged until the session registers`,
  );
}

async function routeLongRunning(
  reminder: BashLongRunningPayload,
  _bridge: AftProjectTransport,
): Promise<void> {
  const route = wakeRouteState().routes.get(reminder.session_id);
  if (route) return route.longRunning(reminder);

  // A long-running reminder only says a task is still running; holding it for
  // later delivery could report a task that has finished since, so it is not held.
  warnUnroutedOnce(
    reminder.session_id,
    `[bash_long_running] no OpenCode Location has session ${reminder.session_id} registered; long-running reminder for task ${reminder.task_id} was not delivered`,
  );
}

/**
 * Deliver the completions held for a session that just registered with
 * `route`. The bridge pushes each completion once, so without this a
 * completion that arrived while its Location was not registered (for example
 * during a Location reload) would stay pending and never wake the session.
 */
function deliverUnrouted(sessionID: string, route: WakeRoute): void {
  const state = wakeRouteState();
  state.warnedUnrouted.delete(sessionID);
  const held = state.unrouted.get(sessionID);
  if (!held) return;
  state.unrouted.delete(sessionID);
  for (const { completion, bridge } of held.values()) {
    route.completion(completion, bridge).catch((error: unknown) => {
      warn(
        `[bash_completion] late delivery of task ${completion.task_id} to session ${sessionID} failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    });
  }
}

/** Test-only: forget every route and held completion in the process-wide table. */
export function __resetV2WakeRoutesForTests(): void {
  const state = wakeRouteState();
  state.routes.clear();
  state.unrouted.clear();
  state.warnedUnrouted.clear();
}

const sharedBridgeOptions: Pick<BridgeOptions, "onBashCompletion" | "onBashLongRunning"> = {
  onBashCompletion: routeCompletion,
  onBashLongRunning: routeLongRunning,
};

export interface V2RuntimeConsumer extends V2ToolConsumers {
  readonly bridgeOptions: typeof sharedBridgeOptions;
  dispose(): void;
}

/**
 * Creates the V2 execution consumers for one OpenCode Location. The bridge owns
 * one process-wide `onBashCompletion` callback, so it routes each notification
 * by session to the delivery object owned by that Location's Effect scope.
 */
export function createV2RuntimeConsumer(context: V2WakeHostContext): V2RuntimeConsumer {
  const delivery = new V2SessionDelivery(context.session);
  const sessions = new Set<string>();
  const route: WakeRoute = {
    completion: async (completion, bridge) => {
      const admitted = await Effect.runPromise(
        delivery.completion({
          sessionID: completion.session_id,
          taskIDs: [completion.task_id],
          text: formatSystemReminder([completion]),
          metadata: {
            source: "aft",
            kind: "bash_completion",
            task_ids: [completion.task_id],
            status: completion.status,
          },
        }),
      );
      if (!admitted) return;

      await bridge.send("bash_ack_completions", {
        session_id: completion.session_id,
        task_ids: [completion.task_id],
      });
    },
    longRunning: (reminder) =>
      Effect.runPromise(
        delivery.status({
          sessionID: reminder.session_id,
          text: formatLongRunningReminder([reminder]),
          description: "Background bash status",
          metadata: {
            source: "aft",
            kind: "bash_long_running",
            task_id: reminder.task_id,
          },
        }),
      ),
  };

  const registerSession = (sessionID: string | undefined): void => {
    if (!sessionID) return;
    sessions.add(sessionID);
    const { routes } = wakeRouteState();
    if (routes.get(sessionID) === route) return;
    routes.set(sessionID, route);
    deliverUnrouted(sessionID, route);
  };

  return {
    bridgeOptions: sharedBridgeOptions,
    executeBash: (execution) => {
      registerSession(execution.context.sessionID);
      return executeV2Bash(execution);
    },
    dispose: () => {
      delivery.dispose();
      const { routes } = wakeRouteState();
      // Another Location may have registered the session since; leave its route.
      for (const sessionID of sessions) {
        if (routes.get(sessionID) === route) routes.delete(sessionID);
      }
      sessions.clear();
    },
  };
}
