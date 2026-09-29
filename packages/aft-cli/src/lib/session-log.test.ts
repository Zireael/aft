import { expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readSessionLog } from "./session-log.js";
import type { RecentSession } from "./sessions.js";

const start = Date.parse("2026-01-01T10:00:00.000Z");
const end = Date.parse("2026-01-01T10:05:00.000Z");
const session: RecentSession = {
  id: "ses_target",
  title: "Search performance comparison",
  projectRoot: "/projects/target",
  startedAt: start,
  lastActivity: end,
};

function withLog(fn: (path: string) => void): void {
  const dir = mkdtempSync(join(tmpdir(), "aft-session-log-"));
  try {
    fn(join(dir, "aft-plugin.log"));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("finds older selected-session lines beyond 500 newer lines without leaking their project", () => {
  withLog((path) => {
    const foreign = Array.from(
      { length: 500 },
      (_, i) => `[2026-01-01T12:00:00.000Z] INFO [aft] /projects/foreign line ${i}`,
    );
    writeFileSync(
      path,
      [
        "[2026-01-01T10:01:00.000Z] INFO [aft] [ses_target] target tagged",
        "[2026-01-01T10:02:00.000Z] ERROR [aft] /projects/target failed: target engine",
        "[2026-01-01T10:03:00.000Z] INFO [aft] [ses_other] /projects/target foreign session",
        ...foreign,
      ].join("\n"),
    );
    const result = readSessionLog(path, session);
    expect(result.lines).toEqual([
      "[2026-01-01T10:01:00.000Z] INFO [aft] [ses_target] target tagged",
      "[2026-01-01T10:02:00.000Z] ERROR [aft] /projects/target failed: target engine",
    ]);
    expect(result.errors).toEqual([result.lines[1]]);
    expect(result.lines.join("\n")).not.toContain("/projects/foreign");
    expect(result.boundHit).toBe(false);
  });
});

test("reports no session lines rather than using an unrelated tail", () => {
  withLog((path) => {
    writeFileSync(path, "[2026-01-01T12:00:00.000Z] INFO [aft] /projects/foreign only\n");
    expect(readSessionLog(path, session)).toEqual({ lines: [], errors: [], boundHit: false });
  });
});

test("recognizes Pi bare UUID tags and searches the rotated predecessor", () => {
  withLog((path) => {
    const id = "019e6307-0749-4000-9000-111111111111";
    writeFileSync(`${path}.1`, `[2026-01-01T10:01:00.000Z] INFO [aft] [${id}] pi target\n`);
    writeFileSync(
      path,
      "[2026-01-01T12:00:00.000Z] INFO [aft] [019e6307-0749-4000-9000-222222222222] pi foreign\n",
    );
    expect(readSessionLog(path, { ...session, id }).lines).toEqual([
      `[2026-01-01T10:01:00.000Z] INFO [aft] [${id}] pi target`,
    ]);
  });
});

test("stops at the byte bound and discards a partial line", () => {
  withLog((path) => {
    writeFileSync(path, "[ses_target] older\n[ses_target] newest\n");
    const result = readSessionLog(path, session, 24);
    expect(result.boundHit).toBe(true);
    expect(result.lines).toEqual(["[ses_target] newest"]);
  });
});
