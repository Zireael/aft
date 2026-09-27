import type {
  AftProjectTransport,
  BashCompletedPayload,
  BashLongRunningPayload,
  BridgeOptions,
} from "@cortexkit/aft-bridge";
import { Effect } from "effect";

import { formatLongRunningReminder, formatSystemReminder } from "../bg-notifications.js";
import { executeV2Bash } from "../tools/bash/executor.js";
import type { V2ToolConsumers } from "../tools/definitions/v2.js";
import { type V2SessionAdmission, V2SessionDelivery } from "./session-delivery.js";

export interface V2WakeHostContext {
  readonly session: V2SessionAdmission;
}

interface WakeRoute {
  readonly delivery: V2SessionDelivery;
  readonly sessions: Set<string>;
}

const sessionRoutes = new Map<string, WakeRoute>();

async function routeCompletion(
  completion: BashCompletedPayload,
  bridge: AftProjectTransport,
): Promise<void> {
  const route = sessionRoutes.get(completion.session_id);
  if (!route) return;

  const admitted = await Effect.runPromise(
    route.delivery.completion({
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
}

async function routeLongRunning(
  reminder: BashLongRunningPayload,
  _bridge: AftProjectTransport,
): Promise<void> {
  const route = sessionRoutes.get(reminder.session_id);
  if (!route) return;

  await Effect.runPromise(
    route.delivery.status({
      sessionID: reminder.session_id,
      text: formatLongRunningReminder([reminder]),
      description: "Background bash status",
      metadata: {
        source: "aft",
        kind: "bash_long_running",
        task_id: reminder.task_id,
      },
    }),
  );
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
  const route: WakeRoute = { delivery, sessions: new Set() };

  const registerSession = (sessionID: string | undefined): void => {
    if (!sessionID) return;
    route.sessions.add(sessionID);
    sessionRoutes.set(sessionID, route);
  };

  return {
    bridgeOptions: sharedBridgeOptions,
    executeBash: (execution) => {
      registerSession(execution.context.sessionID);
      return executeV2Bash(execution);
    },
    dispose: () => {
      delivery.dispose();
      for (const sessionID of route.sessions) {
        if (sessionRoutes.get(sessionID) === route) sessionRoutes.delete(sessionID);
      }
      route.sessions.clear();
    },
  };
}
