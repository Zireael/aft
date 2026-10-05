import { describe, expect, test } from "bun:test";
import { mkdtempSync, realpathSync, rmSync, symlinkSync, unlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  __projectRootWorkForTests,
  canonicalizeProjectRoot,
  invalidateProjectRootMemo,
  normalizeWindowsRoot,
  projectRootKeyHash,
} from "../project-identity.js";

describe("project-identity canonicalization", () => {
  test("repeated roots realpath once and retargeted aliases invalidate", () => {
    const a = realpathSync(mkdtempSync(join(tmpdir(), "aft-memo-a-")));
    const b = realpathSync(mkdtempSync(join(tmpdir(), "aft-memo-b-")));
    const parent = realpathSync(mkdtempSync(join(tmpdir(), "aft-memo-link-")));
    const link = join(parent, "link");
    try {
      symlinkSync(a, link);
      const before = __projectRootWorkForTests();
      for (let i = 0; i < 100; i++) expect(canonicalizeProjectRoot(link)).toBe(a);
      expect(__projectRootWorkForTests().realpaths - before.realpaths).toBe(1);
      expect(__projectRootWorkForTests().stats - before.stats).toBe(199);
      unlinkSync(link);
      symlinkSync(b, link);
      expect(canonicalizeProjectRoot(link)).toBe(b);
      rmSync(b, { recursive: true });
      expect(canonicalizeProjectRoot(link)).toBe(link);
      expect(__projectRootWorkForTests().realpaths - before.realpaths).toBe(2);
    } finally {
      invalidateProjectRootMemo(link);
      for (const path of [a, b, parent]) rmSync(path, { recursive: true, force: true });
    }
  });
  test("trailing separators collapse to one identity", () => {
    const root = realpathSync(mkdtempSync(join(tmpdir(), "aft-pid-")));
    try {
      expect(canonicalizeProjectRoot(root)).toBe(canonicalizeProjectRoot(`${root}/`));
      expect(canonicalizeProjectRoot(root)).toBe(canonicalizeProjectRoot(`${root}///`));
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("equivalent . / .. spellings collapse to one identity", () => {
    const root = realpathSync(mkdtempSync(join(tmpdir(), "aft-pid-")));
    try {
      expect(canonicalizeProjectRoot(join(root, "."))).toBe(canonicalizeProjectRoot(root));
      expect(canonicalizeProjectRoot(join(root, "sub", ".."))).toBe(canonicalizeProjectRoot(root));
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("a symlinked root resolves to its target's identity", () => {
    const target = realpathSync(mkdtempSync(join(tmpdir(), "aft-pid-target-")));
    const parent = realpathSync(mkdtempSync(join(tmpdir(), "aft-pid-link-")));
    const link = join(parent, "link");
    try {
      symlinkSync(target, link);
      expect(canonicalizeProjectRoot(link)).toBe(canonicalizeProjectRoot(target));
    } finally {
      rmSync(target, { recursive: true, force: true });
      rmSync(parent, { recursive: true, force: true });
    }
  });

  // The headline fix: RPC port-file scoping (projectRootKeyHash) and bridge
  // routing (canonicalizeProjectRoot) must agree on identity. The OLD
  // projectHash hashed the raw string, so a symlinked / raw-spelled launch dir
  // scoped its port file to a different directory than the bridge routed to —
  // leaving the sidebar unable to discover the live server.
  test("port-scope hash matches across symlink + trailing-slash spellings", () => {
    const target = realpathSync(mkdtempSync(join(tmpdir(), "aft-pid-hash-")));
    const parent = realpathSync(mkdtempSync(join(tmpdir(), "aft-pid-hashlink-")));
    const link = join(parent, "link");
    try {
      symlinkSync(target, link);
      const viaTarget = projectRootKeyHash(target);
      const viaLink = projectRootKeyHash(link);
      const viaTrailing = projectRootKeyHash(`${target}/`);
      expect(viaLink).toBe(viaTarget);
      expect(viaTrailing).toBe(viaTarget);
      expect(viaTarget).toMatch(/^[0-9a-f]{16}$/);
    } finally {
      rmSync(target, { recursive: true, force: true });
      rmSync(parent, { recursive: true, force: true });
    }
  });

  test("distinct roots get distinct identities and hashes", () => {
    const a = realpathSync(mkdtempSync(join(tmpdir(), "aft-pid-a-")));
    const b = realpathSync(mkdtempSync(join(tmpdir(), "aft-pid-b-")));
    try {
      expect(canonicalizeProjectRoot(a)).not.toBe(canonicalizeProjectRoot(b));
      expect(projectRootKeyHash(a)).not.toBe(projectRootKeyHash(b));
    } finally {
      rmSync(a, { recursive: true, force: true });
      rmSync(b, { recursive: true, force: true });
    }
  });

  test("non-existent path stays total (lexical fallback, no throw)", () => {
    const missing = join(tmpdir(), "aft-pid-definitely-missing-xyz", "sub", "..");
    expect(() => canonicalizeProjectRoot(missing)).not.toThrow();
    expect(projectRootKeyHash(missing)).toMatch(/^[0-9a-f]{16}$/);
  });

  test("Windows verbatim normalization converts only safe DOS and UNC namespaces", () => {
    const win32 = "win32";
    expect(normalizeWindowsRoot("\\\\?\\c:\\repo", win32)).toBe("C:\\repo");
    expect(normalizeWindowsRoot("\\\\?\\UNC\\server\\share\\repo", win32)).toBe(
      "\\\\server\\share\\repo",
    );
    for (const path of [
      "\\\\?\\Volume{1234}\\repo",
      "\\\\?\\UNC\\\\server\\share",
      "\\\\??\\UNC\\\\server\\share",
      "\\\\?\\C:\\repo\\..\\other",
      "\\\\?\\UNC\\server\\share\\.\\repo",
    ]) {
      expect(normalizeWindowsRoot(path, win32)).toBe(path);
    }
  });
});
