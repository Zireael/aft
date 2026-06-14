/**
 * Process-level shutdown handlers.
 *
 * Pi's `session_shutdown` event fires on normal session-end paths, but not when
 * the host Node process is killed by SIGTERM/SIGINT/SIGHUP. Without explicit
 * cleanup, OS propagates the signal to our `aft` children through the process
 * group, the bridge's `exit` handler fires, and (before the sibling fix in
 * bridge.ts) it would auto-restart into orphaned processes.
 *
 * This is a mirror of packages/opencode-plugin/src/shutdown-hooks.ts. The
 * `globalThis` guard ensures OS-level signal handlers are installed exactly
 * once per Node process, even if the Pi extension loads multiple times.
 */

import { log } from "./logger.js";

type Cleanup = () => Promise<void> | void;

interface GlobalState {
  cleanups: Set<Cleanup>;
  installed: boolean;
}

const GLOBAL_KEY = "__aftPiShutdownHooks__";

function getState(): GlobalState {
  const g = globalThis as unknown as Record<string, GlobalState | undefined>;
  if (!g[GLOBAL_KEY]) {
    g[GLOBAL_KEY] = { cleanups: new Set(), installed: false };
  }
  // biome-ignore lint/style/noNonNullAssertion: just initialized above
  return g[GLOBAL_KEY]!;
}

let shuttingDown = false;

async function runCleanups(reason: string): Promise<void> {
  if (shuttingDown) return;
  shuttingDown = true;
  const state = getState();
  if (state.cleanups.size === 0) return;
  log(`Shutdown triggered by ${reason} — running ${state.cleanups.size} cleanup(s)`);
  const cleanups = Array.from(state.cleanups);
  state.cleanups.clear();
  await Promise.allSettled(
    cleanups.map(async (fn) => {
      try {
        await fn();
      } catch (err) {
        log(`Cleanup error: ${(err as Error).message}`);
      }
    }),
  );
}

/** Conventional exit codes for fatal signals (128 + signal number). */
export const SIGNAL_EXIT_CODES = { SIGINT: 130, SIGTERM: 143, SIGHUP: 129 } as const;

/** Cap on cleanup time once WE own process termination for a signal. */
const SIGNAL_CLEANUP_TIMEOUT_MS = 5_000;

/**
 * Registering ANY listener for SIGINT/SIGTERM/SIGHUP disables Node/Bun's
 * default terminate-on-signal behavior. If AFT's listener is the only one,
 * the default was suppressed solely by us, so we must exit or the host hangs
 * forever. If the host or another extension also listens, terminating is
 * their call. Mirror of the OpenCode plugin's shutdown-hooks contract.
 */
export function shouldForceExit(otherListenerCount: number): boolean {
  return otherListenerCount === 0;
}

let signalShutdownStarted = false;

function installProcessHandlers(): void {
  const state = getState();
  if (state.installed) return;
  state.installed = true;

  const signals = ["SIGTERM", "SIGINT", "SIGHUP"] as const;
  for (const sig of signals) {
    const handler = () => {
      const others = process.listenerCount(sig) - 1;
      if (!shouldForceExit(others)) {
        // Named deferral: a shutdown hang is then attributable to the OTHER
        // listener from the log alone.
        const names = process
          .listeners(sig)
          .filter((fn) => fn !== handler)
          .map((fn) => fn.name || fn.toString().slice(0, 80).replace(/\s+/g, " "))
          .join(" | ");
        log(
          `${sig}: deferring termination to ${others} other listener(s); cleanup only. Others: ${names}`,
        );
        void runCleanups(sig);
        return;
      }
      if (signalShutdownStarted) {
        process.exit(SIGNAL_EXIT_CODES[sig]);
      }
      signalShutdownStarted = true;
      // Record the intended code immediately so even an unexpected early exit
      // (event loop drains mid-cleanup) reports the signal, not 0.
      process.exitCode = SIGNAL_EXIT_CODES[sig];
      // Deliberately ref'd: we own termination here, so keeping the process
      // alive for at most the cap is the point — an unref'd timer would let
      // the loop drain and exit before the race settles.
      const timeout = new Promise<void>((resolve) => {
        setTimeout(resolve, SIGNAL_CLEANUP_TIMEOUT_MS);
      });
      void Promise.race([runCleanups(sig), timeout]).finally(() => {
        process.exit(SIGNAL_EXIT_CODES[sig]);
      });
    };
    process.on(sig, handler);
  }
  process.on("beforeExit", () => {
    void runCleanups("beforeExit");
  });
}

/** Register a shutdown cleanup. Returns an unregister function. */
export function registerShutdownCleanup(fn: Cleanup): () => void {
  installProcessHandlers();
  const state = getState();
  state.cleanups.add(fn);
  return () => {
    state.cleanups.delete(fn);
  };
}

export function __shutdownCleanupCountForTests(): number {
  return getState().cleanups.size;
}

export function __resetShutdownCleanupsForTests(): void {
  getState().cleanups.clear();
  shuttingDown = false;
}
