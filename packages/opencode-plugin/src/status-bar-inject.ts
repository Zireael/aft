// Per-session emit-on-change state for the agent status bar (OpenCode).
//
// The Rust bridge attaches current counts to every response; BinaryBridge
// caches the freshest (`getStatusBar()`). Here we gate per session so the bar
// is appended to a tool result only when a value changed (plus a heartbeat),
// keeping the transcript from accumulating identical bars.

import {
  createStatusBarEmitState,
  type StatusBarCounts,
  type StatusBarEmitState,
  shouldEmitStatusBar,
  statusBarLine,
} from "@cortexkit/aft-bridge";

const emitStateBySession = new Map<string, StatusBarEmitState>();

/**
 * Decide whether to surface the bar for this session+counts and, if so, return
 * the line to append to the tool output. Returns `""` when suppressed
 * (unchanged since last emit and heartbeat not elapsed) or when counts are
 * absent (no inspect scan has populated Tier-2 yet).
 */
export function statusBarSuffixForSession(
  sessionID: string,
  counts: StatusBarCounts | undefined,
  force = false,
): string {
  if (!counts) return "";
  let state = emitStateBySession.get(sessionID);
  if (!state) {
    state = createStatusBarEmitState();
    emitStateBySession.set(sessionID, state);
  }
  // `force` guarantees the bar on a tool whose whole purpose is the health
  // glance (aft_inspect): the bar IS that call's summary line, so it must show
  // even when counts are unchanged since the last emit. Still record it as the
  // latest emit + reset the heartbeat so the emit-on-change cadence continues
  // correctly afterward.
  if (force) {
    state.last = counts;
    state.callsSinceEmit = 0;
    return statusBarLine(counts);
  }
  return shouldEmitStatusBar(state, counts) ? statusBarLine(counts) : "";
}

/** Drop a session's emit state (on session delete/shutdown). */
export function clearStatusBarSession(sessionID: string): void {
  emitStateBySession.delete(sessionID);
}
