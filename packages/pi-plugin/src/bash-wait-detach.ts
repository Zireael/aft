import {
  type AftProjectTransport,
  type AftTransportPool,
  containsStandaloneDetachKeyword,
  shouldInterruptWaitsForMessage,
  stripDetachKeywordsAndTidyGap as stripStandaloneDetachKeywords,
} from "@cortexkit/aft-bridge";
import type { AftConfig } from "./config.js";
import { resolveBashConfig } from "./config.js";
import { warn } from "./logger.js";
import { signalSyncWatchAbort } from "./sync-watch-abort.js";

const BASH_TRANSPORT_TIMEOUT_MS = 30_000;

type ActiveBridgePool = Pick<AftTransportPool, "getActiveBridgeForRoot">;

export { BASH_WAIT_DETACH_MAGIC_KEYWORD } from "@cortexkit/aft-bridge";

const EMPTY_DETACH_MESSAGE = "(requested background detach)";

/** Strip the `&detach` control token before Pi's input transform delivers the user message to the model. */
export function stripUserMessageDetachKeyword(messageText: string): string {
  if (!containsStandaloneDetachKeyword(messageText)) return messageText;
  const stripped = stripStandaloneDetachKeywords(messageText);
  return stripped.trim() === "" ? EMPTY_DETACH_MESSAGE : stripped;
}

/**
 * Decide whether a message should detach an active wait:true bash call and
 * abort a sync bash_watch. `messageText` is read before the keyword is
 * stripped from it.
 */
export function shouldDetachBashWaitOnUserMessage(config: AftConfig, messageText: string): boolean {
  return shouldInterruptWaitsForMessage(
    resolveBashConfig(config).detach_on_user_message,
    messageText,
  );
}

/** The fields of Pi's `input` event this module reads. */
export interface PiInputEvent {
  text: string;
}

/**
 * Handle Pi's `input` event for the session's blocking waits: decide before
 * the `&detach` token is stripped, then (only when the decision says so) abort
 * any sync bash_watch and detach any wait:true bash. Both waits follow the same
 * decision, so `bash.detach_on_user_message: false` protects a sync bash_watch
 * as well. Returns the text to deliver to the model.
 */
export function interruptBashWaitsForInput(
  pool: ActiveBridgePool,
  config: AftConfig,
  projectRoot: string,
  sessionID: string | undefined,
  event: PiInputEvent,
): string {
  if (shouldDetachBashWaitOnUserMessage(config, event.text)) {
    signalSyncWatchAbort(sessionID);
    void signalBashWaitDetachForProject(pool, projectRoot, sessionID);
  }
  return stripUserMessageDetachKeyword(event.text);
}

async function sendBashWaitDetach(bridge: AftProjectTransport, sessionID: string): Promise<void> {
  const response = await bridge.send(
    "bash_wait_detach",
    { session_id: sessionID },
    { keepBridgeOnTimeout: true, transportTimeoutMs: BASH_TRANSPORT_TIMEOUT_MS },
  );
  if (response.success === false) {
    throw new Error(String(response.message ?? "bash_wait_detach failed"));
  }
}

export async function signalBashWaitDetachForProject(
  pool: ActiveBridgePool,
  projectRoot: string,
  sessionID: string | undefined,
): Promise<void> {
  if (!sessionID) return;
  const bridge = pool.getActiveBridgeForRoot(projectRoot);
  if (!bridge) return;
  try {
    await sendBashWaitDetach(bridge, sessionID);
  } catch (err) {
    warn(
      `[bash_wait_detach] failed for session ${sessionID}: ${err instanceof Error ? err.message : String(err)}`,
    );
  }
}
