/**
 * The row for issue #387: an OpenCode 2 session created in a linked git
 * worktree must run its tools in that worktree.
 *
 * The host resolves a Location for the session's directory, and that
 * Location's `project.canonical` is the repository's MAIN checkout. AFT used
 * to fall back to that main checkout as its root, so a relative `read` looked
 * there and `bash` ran there. The row reproduces the layout: the scenario's
 * project directory becomes a linked worktree of a main checkout next to it,
 * and the worktree holds a file the main checkout does not have. The host is
 * started in the project directory, so its session lives in the worktree. A
 * scripted `read` of the file's relative path must return its content, and a
 * scripted `pwd` must print the worktree, not the main checkout.
 */
import { cp, mkdir, readFile, realpath, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

import { toolResultForCall } from "../../harness/mock-server.js";
import type { RecordedMockExchange, ScenarioDefinition } from "../../harness/types.js";
import { runCommand } from "../../harness/util.js";

/** Directory name of the main checkout, created next to the scenario's project directory. */
export const MAIN_CHECKOUT_DIRECTORY = "main-checkout";
/** A file that exists only in the linked worktree, read by its relative path. */
export const WORKTREE_ONLY_FILE = "only-in-worktree.txt";
/** The file's single line; an error naming the path does not contain it. */
export const WORKTREE_ONLY_CONTENT = "created-in-linked-worktree";
export const READ_CALL_ID = "linked-worktree-read";
export const PWD_CALL_ID = "linked-worktree-pwd";
/** Prefix the scripted `pwd` command prints before the physical working directory. */
export const PWD_MARKER = "cwd=";

export function isLinkedWorktreeScenario(scenario: ScenarioDefinition): boolean {
  return scenario.metadata?.linked_worktree === true;
}

async function git(args: string[], cwd: string): Promise<void> {
  const output = await runCommand("git", args, { cwd, timeoutMs: 10_000 });
  if (output.exit_code !== 0) {
    throw new Error(
      `linked worktree setup: git ${args.join(" ")} failed in ${cwd}: ${output.stderr.trim() || output.stdout.trim()}`,
    );
  }
}

/**
 * Turn the scenario's project directory, which the harness has already made a
 * committed git repository, into a linked worktree of a main checkout beside it.
 *
 * The repository moves to the main checkout (its `.git` directory is moved and
 * the committed files are checked out there), then `git worktree add` recreates
 * the project directory as a linked worktree on its own branch. Files the
 * harness wrote after its baseline commit, such as the project's AFT config,
 * are carried over into the worktree. The worktree-only file is written last,
 * so the main checkout never has it. Returns the main checkout's path.
 */
export async function prepareLinkedWorktree(projectRoot: string): Promise<string> {
  const parent = dirname(projectRoot);
  const mainCheckout = join(parent, MAIN_CHECKOUT_DIRECTORY);
  const staging = join(parent, "project-before-linked-worktree");
  await rename(projectRoot, staging);
  await mkdir(mainCheckout);
  await rename(join(staging, ".git"), join(mainCheckout, ".git"));
  await git(["reset", "-q", "--hard"], mainCheckout);
  await git(["worktree", "add", "-q", "-b", "linked-worktree", projectRoot], mainCheckout);
  // Anything the harness put in the project after its baseline commit is not
  // in the checked-out branch; copy the staged tree over the new worktree so
  // the project looks exactly as the harness left it.
  await cp(staging, projectRoot, { recursive: true, force: true });
  await rm(staging, { recursive: true, force: true });
  await writeFile(join(projectRoot, WORKTREE_ONLY_FILE), `${WORKTREE_ONLY_CONTENT}\n`);
  return mainCheckout;
}

/**
 * Judge the two scripted calls against the real worktree path, which the
 * registration cannot spell because every run's root differs. The row's shape
 * comparison checks only the last path segment; this checks the whole path.
 */
export async function judgeLinkedWorktreeResults(
  projectRoot: string,
  exchanges: readonly RecordedMockExchange[],
): Promise<void> {
  const worktree = await realpath(projectRoot);
  const mainCheckout = join(dirname(worktree), MAIN_CHECKOUT_DIRECTORY);

  const read = toolResultForCall(exchanges, READ_CALL_ID);
  if (!read) throw new Error(`linked worktree: no tool result for ${READ_CALL_ID}`);
  if (!new RegExp(`^1: ${WORKTREE_ONLY_CONTENT}$`, "m").test(read.text)) {
    throw new Error(
      `linked worktree: read of ${WORKTREE_ONLY_FILE} did not return the worktree file's content, so it did not resolve against the worktree: ${read.text.slice(0, 400)}`,
    );
  }

  const pwd = toolResultForCall(exchanges, PWD_CALL_ID);
  if (!pwd) throw new Error(`linked worktree: no tool result for ${PWD_CALL_ID}`);
  const printed = pwd.text.match(new RegExp(`^${PWD_MARKER}(.*)$`, "m"))?.[1];
  if (printed !== worktree) {
    const where = printed === mainCheckout ? " (the main checkout)" : "";
    throw new Error(
      `linked worktree: bash ran in ${String(printed)}${where}, expected the worktree ${worktree}`,
    );
  }

  // The setup itself is part of the premise: had the main checkout gained the
  // file, a read that resolved against it would pass for the wrong reason.
  const leaked = await readFile(join(mainCheckout, WORKTREE_ONLY_FILE), "utf8").then(
    () => true,
    () => false,
  );
  if (leaked) {
    throw new Error(`linked worktree: the main checkout also has ${WORKTREE_ONLY_FILE}`);
  }
}
