import { spawnSync } from "@cortexkit/aft-bridge";
import { findAftBinary, missingAftBinaryMessage } from "../lib/binary-probe.js";
import { CLI } from "../lib/cli.js";

/**
 * Forward `cache <subcommand>` to the native binary. The Rust command owns
 * argument validation, the dry-run default, the process check and the
 * operator warning, so this wrapper adds no behavior of its own.
 */
export function runCache(argv: string[]): number {
  const binary = findAftBinary();
  if (!binary) {
    console.error(missingAftBinaryMessage(`${CLI} cache`));
    return 1;
  }

  const result = spawnSync(binary, ["cache", ...argv], {
    stdio: "inherit",
    env: process.env,
  });
  if (result.error) {
    console.error(`aft cache failed to start ${binary}: ${result.error.message}`);
    return 1;
  }
  return result.status ?? 1;
}
