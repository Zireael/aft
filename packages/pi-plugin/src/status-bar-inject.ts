// Per-session emit-on-change state for the agent status bar (Pi).
//
// The Rust bridge attaches current counts to every response; BinaryBridge
// caches the freshest (`getStatusBar()`). Here we gate per session so the bar
// is appended to a tool result only when a value changed (plus a heartbeat),
// keeping the transcript from accumulating identical bars. Mirrors the
// OpenCode helper; the only harness difference is where the line is appended
// (Pi tool results are a content-block array).

import {
  createStatusBarEmitState,
  formatStatusBar,
  type StatusBarCounts,
  type StatusBarEmitState,
  shouldEmitStatusBar,
} from "@cortexkit/aft-bridge";

// Pi resolves session IDs as `string | undefined`; an undefined session shares
// one default bucket (Pi is one-session-per-process), mirroring bg-notifications.
const DEFAULT_SESSION = "__default__";

const emitStateBySession = new Map<string, StatusBarEmitState>();

/**
 * Decide whether to surface the bar for this session+counts and, if so, return
 * the formatted bar line (no leading newlines — Pi appends it as its own
 * content block). Returns `undefined` when suppressed (unchanged since last
 * emit and heartbeat not elapsed) or when counts are absent (no inspect scan
 * has populated Tier-2 yet).
 */
export function statusBarBlockForSession(
  sessionID: string | undefined,
  counts: StatusBarCounts | undefined,
  force = false,
): string | undefined {
  if (!counts) return undefined;
  const key = sessionID ?? DEFAULT_SESSION;
  let state = emitStateBySession.get(key);
  if (!state) {
    state = createStatusBarEmitState();
    emitStateBySession.set(key, state);
  }
  // `force` guarantees the bar on aft_inspect — the bar IS that call's summary
  // line, so it must show even when counts are unchanged. Record it as the
  // latest emit + reset the heartbeat so the cadence continues correctly after.
  if (force) {
    state.last = counts;
    state.callsSinceEmit = 0;
    return formatStatusBar(counts);
  }
  return shouldEmitStatusBar(state, counts) ? formatStatusBar(counts) : undefined;
}

/** Drop a session's emit state (on session shutdown). */
export function clearStatusBarSession(sessionID: string | undefined): void {
  emitStateBySession.delete(sessionID ?? DEFAULT_SESSION);
}
