# `aft_delete` recursive refusals: what is refused, and what undo would need (2026-09)

Status: investigation and proposal only. No behaviour changes. Code references are to base `6a21cb509`.

## Summary

- **About a quarter of recursive deletes are refused.** Since the guardrail shipped on 2026-05-16 there have been 248 recursive `aft_delete` calls across OpenCode, Pi and Claude Code. 67 of them (27%) were refused for unsupported contents, covering 79 target directories. Another 5 hit the 30 s tool timeout. The backup budget refusal (2,000 files / 100 MiB) shipped on 2026-09-26 and has not fired yet in the transcripts. Refusals are increasing: 1 target in May, 1 in June, 4 in July, 29 in August and 44 in September up to the 28th.
- **Empty directories cause most refusals.** 73 of the 79 refused targets contain at least one empty directory, and 55 contain nothing else unsupported. 7 targets were an empty directory on its own. Symlinks appear in about 19 targets, nearly all `node_modules/.bin/*` and package links. File-like entries (hard links or symlinks) appear in 7.
- **21 of the 79 refused targets (27%) are in the system temp directory, where undo does not exist anyway.** The backup store skips every path under `/tmp`, `/private/tmp`, `/var/folders/…/T` and similar (`BackupSkippedReason::TempPath`). The guardrail still refuses these trees to protect an undo that would never be written.
- **The backup format can already store symlinks.** `BackupEntryKind::Symlink` stores the raw link text and `restore_symlink` recreates it without following it. Dangling links work on Unix. `move_file` undo uses it today (`undo_after_move_file_restores_symlink_source_and_removes_destination`). Directories and hard links have no representation.
- **Proposal, in two phases:**
  1. When backups are off for the whole tree (temp path, or backups disabled), skip the undo-shape guardrail and the budget. This needs no format change.
  2. Add a `directory` entry kind. Record symlinks with the existing kind, but store the target byte-exact. Handle hard links. Delete from the recorded manifest instead of `remove_dir_all`.

  Mount points, sockets, FIFOs and device nodes stay refused. The 2,000-entry and 100 MiB caps stay, with directories counted as entries.

## 1. What is refused in practice

### Sources

- **Daemon logs** (`~/.local/share/cortexkit/aft/logs/aft-*.log`, 1,341 files): there are no `delete_file` outcome lines. Refusals are returned to the caller and never logged, so the logs cannot be used for this count.
- **Plugin log** (`aft-plugin.log*`): has 10 `perf tool=aft_delete total=…ms` lines, with timing only and no outcome.
- **Transcripts were used instead.** They record every tool call's input and output:
  - OpenCode: the `part` table of `~/.local/share/opencode/opencode.db`, rows with `"type":"tool"` and `"tool":"aft_delete"`. 855 calls from 2026-03-18 to 2026-09-28.
  - Pi: `~/.pi/agent/sessions/**/*.jsonl`, `toolResult` messages with `toolName: "aft_delete"`. 103 calls.
  - Claude Code: `~/.claude/projects/**/*.jsonl`, `mcp__subc__aft_delete`. 1 call.

Refusals were matched on the guardrail text `does not yet support directory trees`. The single-file refusals `symlink undo is not supported` and `refusing to delete hard-linked` were also matched, as was the budget code `recursive_delete_backup_too_large`. The window starts at 2026-05-16, the date of commit `b682d98e9` ("reject recursive delete on symlinks and empty directories").

### Counts

| Measure | Count |
|---|---:|
| Recursive `aft_delete` calls since 2026-05-16 | 248 (OpenCode 219, Pi 28, Claude Code 1) |
| Calls refused for unsupported contents | 67 (27%): OpenCode 55, Pi 12 |
| Refused targets (a batch call can refuse several) | 79 (78 unique paths) |
| Refused targets under system temp (no undo possible) | 21 |
| Calls that hit the 30 s tool timeout | 5 (2026-09-05, 09-06, 09-10, 09-23, 09-26), all before the budget cap existed |
| Budget refusals (`recursive_delete_backup_too_large`) | 0 (the cap shipped on 2026-09-26) |
| A symlink passed directly as the target and refused (all time) | 7 (6 non-recursive calls, 1 inside a recursive batch) |
| Offending paths named in refusal messages | 305, plus 19,049 hidden behind `… and N more` |

The size of the refusal, measured as offending entries per refused target:

| Offending entries per target | Targets |
|---|---:|
| ≤ 5 | 31 |
| ≤ 20 | 57 |
| > 500 | 6 |
| > 2,000 (would now hit the budget cap first) | 3 |

Most refused trees are small, with one or two stray empty directories. After the proposed change they would be accepted. The largest (for example a worktree `.tmp` with 7,557 hidden offenders, and `gpui-chat` with 4,820) would still be refused, now by the budget. That is intended.

### What kind of entry caused the refusal

The refusal message names at most five paths and does not say what kind each one is. At survey time only 5 of the named paths still existed; `lstat` showed they were all empty directories. The rest were classified by name:

- `node_modules/.bin/*` or a top-level package entry: symlink.
- An extensionless name such as `target/release/incremental`, `build/*/out`, `.git/objects/pack`, `.git/refs/tags`, `state/opencode/locks`, `.npm/_cacache/tmp` or `xdg-runtime`: empty directory.
- A name with a file extension: a hard link or symlink.

The classification is approximate, but the pattern is consistent:

| Unsupported kinds present in the target | Targets |
|---|---:|
| empty directories only | 55 |
| empty directories and symlinks | 14 |
| symlinks only | 3 |
| file-like (hard link or symlink) only | 3 |
| empty directories and file-like | 2 |
| empty directories, file-like and symlinks | 2 |

### Typical refused trees

- **E2E and probe scratch homes** (27 targets). Examples are `…/T/anthropic-auth-e2e-<id>` (12 times), worktree `.tmp/opencode2-e2e/…/scenarios/*/{home,xdg-runtime}`, and `.gate-fable.*`. They contain empty `cache/opencode/bin`, `state/opencode/locks` and `data/opencode/repos` directories, plus `config/node_modules/.bin/*` symlinks.
- **Cargo `target/` output** (15 targets), for example `target/r119-bench`, `target/capture-probe`, `target-metal` and `gpui-chat/target/release`. They contain empty `incremental/`, `examples/`, `build/*/out` and `tmp/` directories.
- **`node_modules`** (20 targets): `.bin/*` symlinks, bun `.bun/<pkg>` link farms, and `file:` dependencies.
- **Nested `.git` directories** (9 targets): empty `objects/info`, `objects/pack` and `refs/tags`, and on macOS the fsmonitor socket `.git/fsmonitor--daemon.ipc`.
- **A single empty directory** (7 targets), for example `src/tools/switch-agent` and `.cortexkit/state/e2e-stamps`.

### Census of real trees on this machine

These were walked the way the guardrail walks: no symlink following, and no crossing devices.

| Tree | Files | Bytes | Dirs | Empty dirs | Symlinks | Symlinks leaving the tree | Hard-linked files | Sockets |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `broca/node_modules` (npm) | 548 | 5.4 MiB | 25 | 0 | 2 | 0 | 0 | 0 |
| `cortexkit-e2e/node_modules` (bun, `file:` deps) | 1,921 | 34.9 MiB | 239 | 3 | 673 | 668 (absolute, into `subconscious/…/dist`) | 0 | 0 |
| `callosum/node_modules` | 3,345 | 362 MiB | 692 | 0 | 239 (203 to directories) | 0 | 2 (1 inode, both links inside) | 0 |
| `aft/node_modules` (bun isolated) | 72,200 | 1.4 GiB | 8,184 | 1 | 2,417 (2,213 to directories) | 0 | 6 (the other link is outside, in the `subconscious` repo) | 0 |
| `ProgramBench/.venv` (uv) | 1,942 | 34.3 MiB | 198 | 0 | 3 (`bin/python` → absolute uv interpreter) | 1 | 0 | 0 |
| `cortexkit-account/target/debug` | 1,052 | 148 MiB | 226 | 13 | 0 | 0 | 0 | 0 |
| `broca/.git` | 1,804 | 22.3 MiB | 296 | 9 | 0 | 0 | 0 | 1 (`fsmonitor--daemon.ipc`) |

Two patterns matter for safety. Bun `file:` dependencies are **absolute symlinks into another repository**. Bun also **hard-links files into another repository's working tree**. A delete that followed either kind would destroy or copy another project's sources.

## 2. What undo would need, by kind

This section starts with how the backup store works today, because it affects the cost of every option. Each backed-up path gets its own stack directory under the session directory. Every entry writes a content file (temp file, fsync, rename), rewrites `meta.json` (temp file, fsync, rename), fsyncs the directory 2 or 3 times, and adds one `backups` row to `aft.db`. That cost is paid per entry, whatever its size. A symlink entry already writes its target as the content file.

### Empty directories, and directories in general

- **Today:** `validate_directory_entries` refuses any directory with no entries, including the root itself. Undo does recreate the *parents* of restored files, using `create_dir_all` with the default umask. So directory modes are already lost for non-empty directories, and empty ones are not recorded at all.
- **What undo needs:** a new entry kind `directory` with no content file, carrying `mode`. Restoring it is `mkdir`:
  - If a directory already exists at the path, the entry is satisfied.
  - If a file or symlink exists there, it is a conflict, and the whole operation rolls back as the restore code already does for write failures.
  - Modes are applied last, deepest directory first, so that a read-only directory such as `0555` does not block restoring its children.
- **Should every directory be recorded, or only empty ones?** Recording only empty directories keeps the entry count low, but the restored tree still loses non-empty directory modes, as it does today. Recording every directory makes the restored tree exact, and costs one entry per directory: directories are 4–18% of all entries in the census trees. **Recommendation: record every directory.** That makes the undo result exact and gives the undo code one ordered list of directories to create and to roll back. Directory entries count against the 2,000-entry cap.

### Symlinks

- **Today:** refused inside a recursive tree. The single-file path also refuses a symlink, with the message "symlink undo is not supported", which is out of date. The store already supports symlinks:
  - `backup_entry_from_path` uses `symlink_metadata` and `read_link` and never opens the target.
  - `restore_symlink` removes whatever is at the path and calls `symlink(target, path)`.
  - On Unix, `symlink()` does not check that the target exists, so dangling links come back as they were.
  - Relative targets are stored as written (`read_link` returns the raw text), so `../typescript/bin/tsc` comes back relative.
- **What still has to change:**
  1. **Store the target byte-exact.** Link targets are serialized with `display().to_string()` and `to_string_lossy()` (`entry_meta_json`, `restore_metadata_json`, `content_bytes_for_disk`). A target that is not valid UTF-8 would come back altered. Either store the raw `OsStr` bytes (hex or base64 in `link_target_bytes`), or refuse such links. Refusing is simpler, and none were seen in practice.
  2. **Windows.** `create_symlink` picks a file or directory link with `target.is_dir()`. For a relative target this is evaluated against the daemon's working directory, and a dangling target always reads as a file link. So the link type must be recorded at backup time. `checkpoint.rs` already does this with `create_symlink(target, link, target_is_dir)`. Junctions and other reparse points must be classified explicitly and tested. Until then, keep symlinks refused on Windows.
  3. **Budget:** symlinks already count toward the file cap, at 0 bytes.
  4. **Single-file symlink delete** can use the same entry. This removes the 7 single-file refusals.
- **Rejected alternative:** backing up the target's *content* would be wrong, and dangerous for links that leave the tree (see section 3).

### Hard links

- **Today:** any regular file with `nlink > 1` is refused, in both the single-file and the recursive path.
- **Two cases:**
  1. **Every link to the inode is inside the tree** (the walk sees `nlink` paths for the same `(dev, ino)`). Back up the content once, on the first path. Record each other path as a new kind `hardlink` with `link_to: <first key>`. On undo, restore the first path, then `link()` the others. The restored tree has the same topology. Bytes count once toward the budget, and each path counts as an entry.
  2. **Some links are outside the tree.** Examples are the bun `file:` hard links into `subconscious/…` and pnpm or cargo stores. Deleting the in-tree link leaves the outside copy intact. Undo cannot relink to it without knowing where it is, and a filesystem-wide inode search is not acceptable. Undo can restore the correct *content* as an independent file, but the shared inode is lost, so later edits through the outside link would no longer show up here. **Options:** (a) accept, restore as a copy, and warn in both the delete result ("undo restores N hard-linked files as independent copies") and the undo result; or (b) keep refusing. **Recommendation: (a).** The content is exact, and the lost sharing is what reinstalling the package or rebuilding recreates anyway. This is an operator decision because it weakens "undo restores exactly".

### Directories on another filesystem (mount points)

These stay refused. `std::fs::remove_dir_all` does not stop at a mount point: it deletes the mounted filesystem's contents. The mounted filesystem may be a bind mount of data that lives elsewhere, a tmpfs, a network share, or a container volume, and backing it up is neither bounded nor meaningful. `DeviceBoundary` exists so that neither walk crosses into it; the root cause was the panic `ReadDir::drop` raises with `ENXIO` on a vanished mount. None of the refused targets in the transcripts were mounts. The message tells the caller to unmount first, or to use bash.

### Sockets, FIFOs and device nodes

- **Sockets** stay refused. A socket file holds no data. It is the meeting point for a live process: git fsmonitor, watchman, gpg-agent, or a language server. `bind()` could recreate the path, but with no listener behind it, so clients would see `ECONNREFUSED` instead of `ENOENT`. For fsmonitor that turns "daemon not running" into "daemon broken". Undo cannot bring this back honestly.
  - **Open question for the operator:** nested `.git` directories on macOS usually contain `fsmonitor--daemon.ipc`, so deleting a scratch repo is refused for this one file. Option: accept sockets as "not restored, no data lost", with a per-path warning, in the same way files over 64 MiB are already deleted with an "undo unavailable (too_large)" warning. The default in this proposal is to keep them refused.
- **FIFOs** could technically be restored with `mkfifo` and the recorded mode, since any data in flight is not state. None were seen in practice, so they stay refused for now. They are cheap to add later as a metadata kind.
- **Device nodes** stay refused. Recreating one needs `mknod` with privileges the daemon does not have, so undo would fail.

### Size limits (2,000 entries / 100 MiB per call)

- **Keep both caps.** They exist because the backup copy holds the root's write lane: a 12,000-file tree once copied at about 4 MB/s, well past the 30 s tool timeout. The 5 timeouts in the transcripts are the same failure.
- **Count directories and `hardlink` entries toward the 2,000-entry cap,** as symlinks already are. The per-entry cost is dominated by fsyncs, not bytes, so an entry with no content costs almost as much as a small file. Count bytes once per hard-linked inode.
- **Measure before raising the cap.** Once the format change exists, time the per-entry cost of a metadata-only entry. If batching fsyncs or DB inserts per operation makes those entries cheap, a separate, higher cap for metadata-only entries can follow. The current evidence does not justify raising it now.
- **When nothing will be backed up, apply no budget.** `RecursiveDeleteBackupBudget::enabled` looks only at `policy.enabled` and `max_file_size`, not at the temp-path skip. So a 3,000-file tree under `/tmp` is refused for a copy that would never happen.
- **Expected effect:** the trees agents most often delete are small scratch or e2e homes and small `target/<probe>` outputs, and the change accepts those. Real `node_modules`, `.venv` and full `target/` trees usually exceed 2,000 entries and will keep being refused with the "delete in smaller pieces, or use bash `rm -rf`" message. In the census, `cortexkit-e2e/node_modules` alone has 2,833 entries.

### Trees with no backup (temp paths, backups disabled)

This case was not in the brief, but it causes 27% of refusals. The guardrail's only purpose is to keep undo atomic. When the whole tree is under a system temp root, or backups are disabled, every entry is skipped by `should_snapshot_path` and undo reports "undo unavailable (temp_path)". Refusing on empty directories, symlinks, hard links, sockets or FIFOs protects nothing in that case. The mount-point refusal must stay, because it protects another filesystem, not the undo.

## 3. Safety

- **Neither walk follows links.** Both `collect_files_into` and `validate_directory_entries` use `DirEntry::file_type()`, which does not follow symlinks, so a symlink to a directory is never entered. Backup uses `symlink_metadata` and `read_link` only (`edit::auto_backup`, `backup_entry_from_path`). The implementation must keep this: never call `metadata()`, `canonicalize()`, `is_dir()` or `exists()` on a tree entry, because all of them follow links. The 668 absolute links from `cortexkit-e2e/node_modules` into `subconscious/clients/subc-client/dist` show what following would do: it would back up another repository's files, or with a following delete, destroy them.
- **The delete itself.** `std::fs::remove_dir_all` unlinks symlinks without following them (hardened in Rust 1.58.1, CVE-2022-21658). It does not stop at mount points, which is why mounts must stay refused.
- **Race between check and delete.** Validation, backup and `remove_dir_all` run in sequence, so anything created inside the tree after the walk is deleted without a backup. **Proposal:** delete from the recorded manifest instead. Unlink each recorded non-directory entry, then `rmdir` each recorded directory, deepest first, using `unlinkat` or `rmdir` relative to directory handles that were opened without following links. An entry that appears later makes `rmdir` fail with `ENOTEMPTY`. The delete then stops and reports a partial delete; the existing discard logic already handles that. Nothing unbacked is ever deleted.
- **Path restriction (`validate_path`).** The root is checked with `validate_write_location`, which resolves every ancestor but not the final component, so a symlinked parent cannot escape. Restriction is off by default (`restrict_to_project_root: false`). It is forced for untrusted binds (`subc/mod.rs`, `with_force_restrict`). Entries inside the tree need no separate check as long as the walk does not follow links, because every entry is then physically under the validated root.
  - Symlink *targets* are data, not write locations, and must **not** be validated. Real trees contain legitimate targets outside the project: `.venv/bin/python` points to the uv interpreter, and bun links point into other repositories. Recreating such a link writes nothing outside the root. Any later AFT write through it goes through `validate_path` again.
  - Whole-operation undo (`restore_last_operation`) does not re-validate its keys. It writes to the canonical keys recorded at delete time, which were under the validated root.
- **Undo following a re-planted ancestor.** On restore, the parents of a file are created with `create_dir_all`, then the file is written with `fs::write`, and both follow symlinks. If an ancestor inside the deleted root were replaced by a symlink between the delete and the undo, the restore would write through it. This risk exists today for every restored file. With `directory` entries, undo creates each directory itself, so it can `lstat` every ancestor below the deleted root. If one is not a real directory, undo refuses and rolls back.
- **Recreating links exactly.** Relative and absolute targets and dangling links are already kept on Unix. What remains is the non-UTF-8 gap and the Windows link-type gap described in section 2.
- **The sandbox does not apply.** `aft_delete` runs inside the daemon, not in the bash sandbox (`sandbox_spawn.rs`), so the sandbox imposes no limits on it. Its only fences are the project-root restriction above and the checks in `delete_file.rs`. Nothing in this proposal widens where AFT may write.

## 4. Proposal

### What `aft_delete` would accept

| Entry in the tree | Today | Phase 1 (no format change) | Phase 2 (format change) |
|---|---|---|---|
| Any entry, when the whole tree is under system temp or backups are disabled | refused if unsupported, and budget applies | **accepted**, no budget; mounts still refused | same |
| Empty directory, including the root | refused | refused (backed-up trees) | **accepted**, `directory` entry |
| Directory mode | lost on undo | unchanged | **restored** |
| Symlink (Unix) with a UTF-8 target, including dangling and out-of-tree targets | refused | refused | **accepted**, `symlink` entry, target not followed |
| Symlink with a non-UTF-8 target | refused | refused | refused, unless `link_target_bytes` is stored |
| Symlink on Windows | refused | refused | refused until the link type is recorded and tested |
| Hard link, all links inside the tree | refused | refused | **accepted**, content once plus `hardlink` entries, relinked on undo |
| Hard link with links outside the tree | refused | refused | **accepted with a warning**, restored as a copy (operator decision) |
| Single-file delete of a symlink or hard link | refused | refused | **accepted**, same entries |
| Mount point / other filesystem | refused | refused | refused |
| Socket | refused | refused | refused (open question above) |
| FIFO, device node | refused | refused | refused |
| Tree over 2,000 entries or 100 MiB | refused | refused (backed-up trees only) | refused; directories and hard links now count as entries |

Phase 1 covers 21 of the 79 observed refusals and involves no format change. Phase 2 covers almost all of the rest (empty directories and symlinks), except trees that exceed the budget and trees holding a socket.

### Changes to the backup and undo format

The format is persisted: per-stack `meta.json` with `schema_version: 4` and `format_version: "v2"`, and the `backups` table in `aft.db` with its `kind` column and `restore_meta` JSON (`DB_RESTORE_META_VERSION = 1`).

- **New `kind` values:**
  - `"directory"`: no `content_path`, no `backup_path`, `mode` set.
  - `"hardlink"`: no `content_path`, no `backup_path`, and a new field `link_to` holding the canonical key of the entry that carries the content.
- **Symlink entries:**
  - add `link_target_bytes`, the raw bytes (only if non-UTF-8 targets are to be supported);
  - add `link_type` (`"file"`, `"dir"` or `"junction"`) for Windows.
- **`restore_meta`:** carries the same new fields, which bumps `DB_RESTORE_META_VERSION` to 2.
- **`meta.json`:** `SCHEMA_VERSION` goes to 5.
- **Restore order within one operation:**
  1. directories, parent first;
  2. content entries;
  3. hard links;
  4. symlinks;
  5. tombstones, as today;
  6. directory modes, deepest first.

  If any step fails, everything is rolled back, extending the existing `rollback_created_dirs` and `rollback_transactional_restore`.

**Rollback (running an older daemon over a newer store).** Older readers do not check `schema_version`. `entry_kind_from_meta` and `TryFrom<BackupRow>` read any unknown `kind` as `Content`, then require a content file:

- On the disk path, `content_path` is missing, which is an error.
- On the DB path, `backup_path` is `None`, which gives the error "has no backup_path".

`corrupt_v2_meta_fails_closed_for_operation_and_single_restore` shows that a stack which cannot be read makes both whole-operation undo and single-file undo fail with `io_error`, leaving the disk untouched. So an older daemon refuses to undo a phase 2 recursive delete, and does **not** restore the directory or hard link as an empty file. This holds only while the new kinds **never** carry `content_path` or `backup_path`, and the implementation must pin that with a test: write a `directory` and a `hardlink` entry, load them through the current v4 reader code, and assert that undo fails and nothing is written. Every other stack keeps working under either version. Stacks written by the older version load unchanged in the newer one, because the change only adds kinds and fields. The newer reader must also accept `restore_meta` versions 1 and 2. Today, `restore_metadata_from_json` rejects any version but 1 and falls back to the disk metadata.

### Tests whose meaning changes

These tests assert the current refusals. They would be rewritten on purpose, and each commit message must say so:

- `empty_subdir_blocks_recursive_delete`
- `symlink_to_outside_file_blocks_recursive_delete`
- `symlink_to_directory_blocks_recursive_delete`
- `hard_link_blocks_recursive_delete`
- `symlink_file_delete_is_rejected_without_project_restriction`
- `symlink_file_delete_is_rejected_with_project_restriction`
- `relative_symlink_file_delete_is_rejected_with_project_restriction`

All of these are in `crates/aft/tests/integration/safety_test.rs`. The symlink tests should become "accepted, target untouched, undo recreates the link text exactly, including dangling". `unix_socket_blocks_recursive_delete` and `recursive_delete_refuses_tree_over_backup_budget_without_deleting` keep their meaning.

### Decisions for the operator

1. Approve phase 1: no guardrail and no budget when nothing will be backed up. Mounts stay refused.
2. Approve phase 2: the format change above, with the version bumps and fail-closed rollback.
3. Hard links with links outside the tree: restore as a copy with a warning (recommended), or keep refusing?
4. Sockets, for example the `.git` fsmonitor socket: keep refusing (the default), or accept as "not restored" with a warning?
5. Non-UTF-8 symlink targets: store the raw bytes, or refuse (recommended; none seen)?

## Method notes

- The transcript queries match on tool name and output text. Outputs cut off by the harness, or reformatted into prose by the plugin, could hide a refusal. A search of every error without a matched refusal found none that was a guardrail refusal.
- The window ends 2026-09-28 at 14:35 local time.
- Nothing measured here tracks what agents did after a refusal. It was not checked whether they fell back to bash `rm -rf`.
- A separate finding, not addressed here: 4 recursive calls passed `recursive: "true"` as a string, with `files` as a JSON-encoded string. The daemon's `as_bool()` reads that as `false`, and the caller gets "… is a directory. Pass recursive: true", which is misleading because they did pass it. One of these targets was `~/.config/cortexkit`.
- The daemon does not log refusals. A `log::info!` with the refusal code and a count of offending entries by kind would make a later re-measure a one-line log query instead of a transcript scan.
