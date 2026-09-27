import { spawnSync } from "@cortexkit/aft-bridge";
import { findAftBinary, missingAftBinaryMessage } from "../lib/binary-probe.js";
import { CLI } from "../lib/cli.js";

/**
 * Forward `backups <subcommand>` to the native binary. The Rust command owns
 * argument validation, the dry-run default, and routing the purge through a
 * running AFT daemon, so this wrapper adds no behavior of its own.
 */
export function runBackups(argv: string[]): number {
  const binary = findAftBinary();
  if (!binary) {
    console.error(missingAftBinaryMessage(`${CLI} backups`));
    return 1;
  }

  const result = spawnSync(binary, ["backups", ...argv], {
    stdio: "inherit",
    env: process.env,
  });
  if (result.error) {
    console.error(`aft backups failed to start ${binary}: ${result.error.message}`);
    return 1;
  }
  return result.status ?? 1;
}
