// Agent status bar — the IDE-style "status bar" surfaced to the agent on tool
// results. The Rust bridge attaches raw counts to every response envelope as a
// top-level `status_bar` object (`Response.data` is `#[serde(flatten)]`, so it
// lands beside `id`/`success`, not nested under `data` — same as
// `bg_completions`); the plugin appends a compact one-line bar to the
// agent-facing tool output on an emit-on-change basis (plus a heartbeat so it
// never scrolls fully out of context).
//
// The legend is taught once in the system prompt (workflow-hints), so the
// per-call cost is just the compact values (~15-20 tokens). ASCII-only — cheaper
// to tokenize and easier for weaker models to parse than unicode glyphs.

/** Raw status-bar counts as attached by the Rust bridge (top-level `status_bar`). */
export interface StatusBarCounts {
  errors: number;
  warnings: number;
  dead_code: number;
  unused_exports: number;
  duplicates: number;
  todos: number;
  /** Tier-2 counts (D/U/C) predate the latest edit; rendered with a `~` marker. */
  tier2_stale: boolean;
}

/**
 * Re-emit the bar after this many tool calls even when nothing changed, so the
 * latest health state doesn't scroll out of the model's recent context during
 * long read-only stretches.
 */
export const STATUS_BAR_HEARTBEAT_CALLS = 15;

/** Per-session emit-on-change state. */
export interface StatusBarEmitState {
  last?: StatusBarCounts;
  callsSinceEmit: number;
}

export function createStatusBarEmitState(): StatusBarEmitState {
  return { callsSinceEmit: 0 };
}

/** Parse/normalize an untrusted top-level `status_bar` payload into counts. */
export function parseStatusBarCounts(value: unknown): StatusBarCounts | undefined {
  if (!value || typeof value !== "object") return undefined;
  const record = value as Record<string, unknown>;
  const num = (key: string): number => {
    const raw = record[key];
    return typeof raw === "number" && Number.isFinite(raw) ? raw : 0;
  };
  return {
    errors: num("errors"),
    warnings: num("warnings"),
    dead_code: num("dead_code"),
    unused_exports: num("unused_exports"),
    duplicates: num("duplicates"),
    todos: num("todos"),
    tier2_stale: record.tier2_stale === true,
  };
}

function countsEqual(a: StatusBarCounts, b: StatusBarCounts): boolean {
  return (
    a.errors === b.errors &&
    a.warnings === b.warnings &&
    a.dead_code === b.dead_code &&
    a.unused_exports === b.unused_exports &&
    a.duplicates === b.duplicates &&
    a.todos === b.todos &&
    a.tier2_stale === b.tier2_stale
  );
}

/**
 * Decide whether to emit the bar this tool call and advance the emit state.
 * Emits when: first observation, any value changed, or the heartbeat elapsed.
 * Mutates `state` (records last-emitted + resets/advances the call counter).
 */
export function shouldEmitStatusBar(state: StatusBarEmitState, next: StatusBarCounts): boolean {
  const changed = state.last === undefined || !countsEqual(state.last, next);
  // Count this call first, so the heartbeat fires on exactly the Nth call since
  // the last emit (N-1 suppressed, then re-emit).
  state.callsSinceEmit += 1;
  const heartbeat = state.callsSinceEmit >= STATUS_BAR_HEARTBEAT_CALLS;
  if (changed || heartbeat) {
    state.last = next;
    state.callsSinceEmit = 0;
    return true;
  }
  return false;
}

/**
 * Render the compact one-line bar. Always shows every field (consistent shape
 * is easier for weak models to parse than a variable one). A `~` before the
 * dead-code count marks the Tier-2 counts (D/U/C) as stale — edited since the
 * last background scan; run aft_inspect for current numbers.
 *
 * Example: `[AFT E2 W5 | D331 U221 C1159 | T8]`
 * Stale:   `[AFT E2 W5 | ~D331 U221 C1159 | T8]`
 */
export function formatStatusBar(counts: StatusBarCounts): string {
  const staleMark = counts.tier2_stale ? "~" : "";
  return (
    `[AFT E${counts.errors} W${counts.warnings} | ` +
    `${staleMark}D${counts.dead_code} U${counts.unused_exports} C${counts.duplicates} | ` +
    `T${counts.todos}]`
  );
}

/** The two-space-prefixed line appended to tool output (matches `[Hint]` style). */
export function statusBarLine(counts: StatusBarCounts): string {
  return `\n\n${formatStatusBar(counts)}`;
}
