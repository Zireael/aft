import { describe, expect, test } from "bun:test";
import { maybeAppendConflictsHint } from "../shared/bash-hints";

describe("maybeAppendConflictsHint", () => {
  // Real `git merge` output for a content conflict.
  test("appends hint on real git-merge conflict output", () => {
    const output = [
      "Auto-merging packages/opencode-plugin/src/index.ts",
      "CONFLICT (content): Merge conflict in packages/opencode-plugin/src/index.ts",
      "Automatic merge failed; fix conflicts and then commit the result.",
    ].join("\n");

    const result = maybeAppendConflictsHint(output);
    expect(result).toContain("[Hint] Use aft_conflicts");
  });

  // Real `git rebase` output where applying a pick failed.
  test("appends hint on rebase conflict output", () => {
    const output = [
      "error: could not apply 0e3f4a2... feat: add foo",
      "Automatic merge failed; fix conflicts and then commit the result.",
    ].join("\n");

    const result = maybeAppendConflictsHint(output);
    expect(result).toContain("[Hint] Use aft_conflicts");
  });

  // The trigger string appears verbatim in many docs/READMEs. The hint
  // must NOT fire just because someone cat'd a README in a non-git
  // directory — otherwise `aft_conflicts` runs and returns "not a git
  // repository", confusing the agent. This is the regression test for
  // user-reported issue (2026-05-22).
  test("does NOT append hint when marker appears alone (e.g. README excerpt)", () => {
    const output =
      "When git can't merge automatically, you'll see:\n\n" +
      "  Automatic merge failed; fix conflicts and then commit the result.\n\n" +
      "This means you need to resolve the conflict manually.";

    const result = maybeAppendConflictsHint(output);
    expect(result).toBe(output);
    expect(result).not.toContain("[Hint]");
  });

  // Defense-in-depth: the conflict-signal regex must require start-of-line.
  // A `CONFLICT (` substring embedded in a message body shouldn't qualify.
  test("does NOT fire on mid-line CONFLICT substring", () => {
    const output = "we documented: Automatic merge failed; fix conflicts. (see CONFLICT (3) below)";

    const result = maybeAppendConflictsHint(output);
    expect(result).toBe(output);
  });

  test("does NOT append hint when output is unrelated to git", () => {
    const output = "hello world\n+0/-0\nlinting passed.";
    const result = maybeAppendConflictsHint(output);
    expect(result).toBe(output);
  });

  test("does NOT append hint when output is empty", () => {
    expect(maybeAppendConflictsHint("")).toBe("");
  });
});
