import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import type { MigrationHarness } from "./migration.js";

export function resolveHarnessStoragePath(
  storageRoot: string,
  harness: MigrationHarness,
  ...segments: string[]
): string {
  return join(storageRoot, harness, ...segments);
}

export function repairRootScopedStorageFile(
  storageRoot: string,
  harness: MigrationHarness,
  fileName: string,
): string {
  const harnessPath = resolveHarnessStoragePath(storageRoot, harness, fileName);
  const rootPath = join(storageRoot, fileName);

  if (existsSync(harnessPath) || !existsSync(rootPath)) return harnessPath;

  try {
    mkdirSync(dirname(harnessPath), { recursive: true });
    renameSync(rootPath, harnessPath);
  } catch {
    // Best-effort compatibility repair. Callers still use the harness path so
    // new writes stop extending the root-scoped layout.
  }

  return harnessPath;
}

/**
 * Decides whether to surface the version-specific announcement dialog/toast.
 *
 * Three cases, all driven off the persisted `last_announced_version` file:
 *
 * 1. **Existing user, same version** — file matches `currentVersion`. Skip.
 *
 * 2. **Existing user, upgrade** — file holds a *different* non-empty version.
 *    Show the dialog so the user sees what's new in their upgrade. After the
 *    dialog is dismissed, the host calls `markAnnouncementSeen` to record
 *    `currentVersion`.
 *
 * 3. **Fresh install or ephemeral sandbox** — file does not exist OR holds
 *    only whitespace. We deliberately do NOT show changelog bullets to a
 *    first-time user (no context to interpret them), AND we don't pester
 *    Docker/CI/disposable-VM users whose storage gets wiped every boot.
 *    Instead we silently **seed** the file with `currentVersion` so the very
 *    next launch behaves like case 1. Future upgrades still trigger case 2.
 *
 * Failures to read/write the marker file are non-fatal: we never let a
 * filesystem hiccup spam an announcement. On any I/O error the function
 * returns `false` and the host treats this turn as already-announced.
 *
 * Returns:
 *   - `true`  → the caller should render the announcement and then call
 *               `markAnnouncementSeen(...)` once the user has seen it.
 *   - `false` → skip rendering. (File was already up-to-date, OR this was a
 *               fresh-install seed and the file has now been written so the
 *               next launch will also skip.)
 */
export function shouldShowAnnouncement(
  storageRoot: string,
  harness: MigrationHarness,
  currentVersion: string,
): boolean {
  if (!currentVersion) return false;

  const versionFile = repairRootScopedStorageFile(storageRoot, harness, "last_announced_version");

  let lastVersion = "";
  try {
    if (existsSync(versionFile)) {
      lastVersion = readFileSync(versionFile, "utf-8").trim();
    }
  } catch {
    // Read failed — be conservative and skip the announcement so a flaky
    // filesystem can't repeatedly flash a dialog.
    return false;
  }

  if (lastVersion === currentVersion) return false;

  if (!lastVersion) {
    // Fresh install or sandbox: silently mark as seen. The next launch sees
    // case 1 (file matches) and stays quiet. Real upgrades from a persisted
    // older version still hit the `lastVersion !== currentVersion` path
    // above and surface the dialog.
    try {
      mkdirSync(dirname(versionFile), { recursive: true });
      writeFileSync(versionFile, currentVersion);
    } catch {
      // Best-effort. If we couldn't seed the file we still skip this turn so
      // the user isn't pestered; we'll just try to seed again next launch.
    }
    return false;
  }

  return true;
}

/**
 * Records that the user has seen `currentVersion`'s announcement. Best-effort
 * filesystem write — failures are silently swallowed because the worst case
 * is repeating the announcement once, not a broken plugin.
 */
export function markAnnouncementSeen(
  storageRoot: string,
  harness: MigrationHarness,
  currentVersion: string,
): void {
  if (!currentVersion) return;

  const versionFile = repairRootScopedStorageFile(storageRoot, harness, "last_announced_version");

  try {
    mkdirSync(dirname(versionFile), { recursive: true });
    writeFileSync(versionFile, currentVersion);
  } catch {
    // Best-effort.
  }
}
