/**
 * Shared NDJSON subprocess helper for AFT benchmarks.
 *
 * The core problem: `spawnSync` with `input` closes stdin after sending all
 * data. AFT's reader thread sees EOF → channel disconnects → main loop exits
 * before the search command finishes. The fix: use `spawn` with stdin kept
 * open, write commands one at a time, and read stdout line-by-line until we
 * get the response we need.
 */

import { spawn, type ChildProcess } from "child_process";
import { createInterface } from "readline";

export interface AftResponse {
  id: string;
  success: boolean;
  [key: string]: unknown;
}

/**
 * Spawn an AFT process, send NDJSON commands, and collect responses.
 *
 * @param binaryPath  Path to the `aft` binary
 * @param commands    Array of NDJSON command objects to send
 * @param timeoutMs   Max time to wait for the final response (default: 30s)
 * @returns Array of parsed JSON responses (one per command)
 */
export function aftNdjson(
  binaryPath: string,
  commands: Record<string, unknown>[],
  timeoutMs = 30000
): Promise<AftResponse[]> {
  return new Promise<AftResponse[]>((resolve, reject) => {
    const child = spawn(binaryPath, [], {
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });

    const responses: AftResponse[] = [];
    let resolved = false;
    const timer = setTimeout(() => {
      if (!resolved) {
        resolved = true;
        child.kill();
        reject(new Error(`aft Ndjson timeout after ${timeoutMs}ms`));
      }
    }, timeoutMs);

    // Read stdout line-by-line
    const rl = createInterface({ input: child.stdout! });
    rl.on("line", (line) => {
      const trimmed = line.trim();
      if (!trimmed) return;
      try {
        const parsed = JSON.parse(trimmed) as AftResponse;
        // Skip push frames (they have "type" not "id")
        if (!parsed.id || parsed.type) return;
        responses.push(parsed);
        // Resolve after we've received all expected command responses
        if (responses.length >= commands.length && !resolved) {
          resolved = true;
          clearTimeout(timer);
          rl.close();
          child.stdin!.end();
          child.on("exit", () => resolve(responses));
          setTimeout(() => {
            if (!resolved) {
              resolved = true;
              child.kill();
              resolve(responses);
            }
          }, 500);
        }
      } catch {
        // Ignore non-JSON lines (stderr noise, etc.)
      }
    });

    child.on("error", (err) => {
      if (!resolved) {
        resolved = true;
        clearTimeout(timer);
        reject(err);
      }
    });

    child.on("exit", () => {
      if (!resolved) {
        resolved = true;
        clearTimeout(timer);
        resolve(responses);
      }
    });

    // Send commands one at a time — each line is a complete NDJSON frame
    for (const cmd of commands) {
      child.stdin!.write(JSON.stringify(cmd) + "\n");
    }
    // Keep stdin open — don't end() it. The process needs to keep reading
    // until it processes all commands. We end it only after we have all responses.
  });
}

/**
 * Synchronous wrapper around aftNdjson for benchmark use.
 * Runs the async function and blocks until complete.
 */
export function aftNdjsonSync(
  binaryPath: string,
  commands: Record<string, unknown>[],
  timeoutMs = 30000
): AftResponse[] {
  // Use spawnSync as fallback for environments without proper async support
  const { spawnSync } = require("child_process");
  const input = commands.map((c) => JSON.stringify(c)).join("\n") + "\n";

  const result = spawnSync(binaryPath, [], {
    input,
    encoding: "utf-8",
    timeout: timeoutMs,
    stdio: "pipe",
  });

  const responses: AftResponse[] = [];
  if (result.stdout) {
    for (const line of result.stdout.split("\n")) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      try {
        responses.push(JSON.parse(trimmed));
      } catch {}
    }
  }
  return responses;
}

// ---------------------------------------------------------------------------
// Persistent AFT session — keeps one process alive across multiple calls.
// Required for semantic search where the model loads asynchronously after
// configure and queries must reuse the same in-process index.
// ---------------------------------------------------------------------------

export class AftSession {
  private child: ReturnType<typeof spawn>;
  private rl: ReturnType<typeof createInterface>;
  private buf: AftResponse[] = [];
  private id = 0;
  private closed = false;

  constructor(binaryPath: string) {
    this.child = spawn(binaryPath, [], {
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    this.rl = createInterface({ input: this.child.stdout! });
    this.rl.on("line", (line) => {
      const trimmed = line.trim();
      if (!trimmed) return;
      try {
        const parsed = JSON.parse(trimmed) as AftResponse;
        // Skip push frames
        if (!parsed.id || parsed.type) return;
        this.buf.push(parsed);
      } catch {}
    });
  }

  /** Send a command and wait for its response. */
  call(command: Record<string, unknown>, timeoutMs = 60000): Promise<AftResponse> {
    return new Promise<AftResponse>((resolve, reject) => {
      this.id++;
      const id = String(this.id);
      const msg = { ...command, id };

      const deadline = Date.now() + timeoutMs;
      const timer = setInterval(() => {
        const idx = this.buf.findIndex((r) => r.id === id);
        if (idx >= 0) {
          clearInterval(timer);
          resolve(this.buf.splice(idx, 1)[0]);
        } else if (Date.now() > deadline) {
          clearInterval(timer);
          reject(new Error(`aft session call ${id} timeout after ${timeoutMs}ms`));
        }
      }, 50);

      this.child.stdin!.write(JSON.stringify(msg) + "\n");
    });
  }

  /** Kill the underlying process. */
  close() {
    if (this.closed) return;
    this.closed = true;
    this.rl.close();
    this.child.stdin!.end();
    this.child.kill();
  }
}
