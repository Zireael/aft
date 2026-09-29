//! Handler for the `delete_file` command: remove file(s) or directory with backup.

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use lsp_types::FileChangeType;
use serde_json::Value;

use crate::commands::delete_tree::{
    delete_recorded_tree, plan_file_backups, walk_tree, BudgetExceeded, BudgetLimit, CollectError,
    FileBackup, NodeKind, RecursiveDeleteBackupBudget, TreeManifest, UnsupportedKind,
};
use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// Handle a `delete_file` request.
///
/// Params:
///   - `file` (string) — single file/dir path
///   - `files` (string[]) — multiple paths (file or dir mixed); takes precedence over `file`
///   - `recursive` (bool, optional, default false) — required to delete a
///     directory. Refuses dir deletion when false to prevent accidental wipes
///     when an agent passes a directory path expecting file semantics.
///
/// All deletes inside a single tool call share one operation id, so a single
/// `aft_safety undo` (without filePath) restores everything atomically.
///
/// A recursive delete whose undo backup would copy more than
/// `RECURSIVE_DELETE_BACKUP_MAX_FILES` files or
/// `RECURSIVE_DELETE_BACKUP_MAX_BYTES` bytes (a budget shared by the whole
/// call) is refused with `recursive_delete_backup_too_large` before anything
/// is deleted.
///
/// Returns single-file: `{ file, deleted, backup_id? }`
/// Returns directory:   `{ file, deleted, is_directory, files_deleted, backup_ids }`
/// Returns batch:       `{ complete, deleted: [...], skipped_files: [...] }`
pub fn handle_delete_file(req: &RawRequest, ctx: &AppContext) -> Response {
    let op_id = crate::backup::new_op_id();
    let recursive = req
        .params
        .get("recursive")
        .and_then(crate::subc_translate::model_boolean)
        .unwrap_or(false);
    let mut budget = budget_for_request(ctx);

    let parsed_files = match req.params.get("files") {
        Some(Value::String(raw)) => match serde_json::from_str::<Vec<String>>(raw) {
            Ok(files) => Some(serde_json::json!(files)),
            Err(_) => {
                return Response::error(
                    &req.id,
                    "invalid_request",
                    "delete_file: 'files' must be an array of paths",
                )
            }
        },
        _ => None,
    };
    // Batch mode: `files: [...]`
    if let Some(files) = parsed_files
        .as_ref()
        .or_else(|| req.params.get("files"))
        .and_then(Value::as_array)
    {
        let mut deleted = Vec::new();
        let mut skipped = Vec::new();
        for value in files {
            let Some(file) = value.as_str() else {
                skipped.push(serde_json::json!({"file": value, "reason": "not a string"}));
                continue;
            };
            match delete_one_or_dir(req, ctx, file, recursive, &op_id, &mut budget) {
                Ok(result) => deleted.push(result),
                Err(resp) => skipped.push(serde_json::json!({
                    "file": file,
                    "reason": resp.data.get("message").and_then(|v| v.as_str()).unwrap_or("delete failed"),
                })),
            }
        }
        if deleted.is_empty() && !skipped.is_empty() {
            let message = format!(
                "delete failed for all {} file(s):\n{}",
                skipped.len(),
                skipped
                    .iter()
                    .map(|entry| {
                        let file = entry
                            .get("file")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or_else(|| entry.get("file").map(Value::to_string))
                            .unwrap_or_default();
                        let reason = entry
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("delete failed");
                        format!("  {file}: {reason}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            return Response::error_with_data(
                req.id.clone(),
                "delete_failed",
                message,
                serde_json::json!({
                    "complete": false,
                    "all_failed": true,
                    "deleted": deleted,
                    "skipped_files": skipped,
                }),
            );
        }
        let mut result = serde_json::json!({
            "complete": skipped.is_empty(),
            "deleted": deleted,
            "skipped_files": skipped,
        });
        edit::attach_backup_skipped_reason(&mut result, ctx, req.session(), &op_id, None);
        return Response::success(&req.id, result);
    }

    // Single-target mode: `file: "..."`
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "delete_file: missing required param 'file' or 'files'",
            );
        }
    };

    match delete_one_or_dir(req, ctx, file, recursive, &op_id, &mut budget) {
        Ok(result) => Response::success(&req.id, result),
        Err(resp) => resp,
    }
}

/// Delete a single path (file or directory). Returns the per-target result
/// payload on success (shape varies for file vs directory), or a ready-made
/// error `Response` on failure for the caller to either propagate (single
/// mode) or aggregate into `skipped_files` (batch mode).
fn delete_one_or_dir(
    req: &RawRequest,
    ctx: &AppContext,
    file: &str,
    recursive: bool,
    op_id: &str,
    budget: &mut RecursiveDeleteBackupBudget,
) -> Result<serde_json::Value, Response> {
    let path = match ctx.validate_write_location(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return Err(resp),
    };

    // Inspect the entry itself, never what a symlink points at: a link must be
    // treated as a link even when its target is a directory or is missing.
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Response::error(
                &req.id,
                "file_not_found",
                format!("delete_file: file not found: {}", file),
            ));
        }
        Err(e) => {
            return Err(Response::error(
                &req.id,
                "io_error",
                format!("delete_file: failed to inspect '{}': {}", file, e),
            ));
        }
    };
    let _view_intent = crate::views::intent::record_paths([path.as_path()]);
    let is_symlink = metadata.file_type().is_symlink();
    let is_dir = metadata.is_dir();
    let no_backup = no_backup_reason(ctx, &path, is_dir);

    if is_symlink && no_backup.is_none() {
        let unsupported = crate::commands::delete_tree::symlink_support(&path).map_err(|e| {
            Response::error(
                &req.id,
                "io_error",
                format!("delete_file: failed to read symlink '{}': {}", file, e),
            )
        })?;
        if let Some(kind) = unsupported {
            log_refusal("invalid_request", &path, &[(kind, 1)]);
            return Err(Response::error(
                &req.id,
                "invalid_request",
                format!(
                    "delete_file: refusing to delete symlink '{}': {}",
                    file,
                    unsupported_symlink_reason(kind)
                ),
            ));
        }
    }

    if is_dir {
        if !recursive {
            return Err(Response::error(
                &req.id,
                "invalid_request",
                format!(
                    "delete_file: '{}' is a directory. Pass recursive: true to delete it with all contents.",
                    file
                ),
            ));
        }
        return delete_directory(req, ctx, &path, file, op_id, budget, no_backup);
    }

    if let Some(reason) = no_backup {
        // Nothing here would be backed up, so there is no undo whose shape a
        // symlink, hard link, or special file could break: delete the entry
        // itself (never a link's target) and report why undo is unavailable.
        return delete_entry_without_backup(req, ctx, &path, file, op_id, reason);
    }

    let mut warnings = Vec::new();
    let backup_id = if is_symlink {
        // The symlink itself is backed up (its target text), never the file
        // it points at.
        edit::auto_backup(
            ctx,
            req.session(),
            &path,
            "delete_file: pre-delete backup",
            Some(op_id),
        )
    } else if metadata.is_file() {
        if file_link_count(&metadata) > 1 {
            // Because the other hard links are outside this delete, undo
            // restores this file as an independent copy that no longer shares
            // changes with those links.
            warnings.push(detached_hard_link_warning(file));
            ctx.backup().lock().snapshot_detached_hard_link_with_op(
                req.session(),
                &path,
                "delete_file: pre-delete backup",
                op_id,
            )
        } else {
            edit::auto_backup(
                ctx,
                req.session(),
                &path,
                "delete_file: pre-delete backup",
                Some(op_id),
            )
        }
    } else {
        match crate::commands::delete_tree::special_file_kind(&metadata.file_type()) {
            None => {
                warnings.push(socket_warning(file));
                Ok(None)
            }
            Some(kind) => {
                log_refusal("unsupported_directory_contents", &path, &[(kind, 1)]);
                return Err(Response::error(
                    &req.id,
                    "unsupported_directory_contents",
                    format!(
                        "delete_file: refusing to delete '{}': {}",
                        file,
                        unsupported_kind_reason(kind)
                    ),
                ));
            }
        }
    }
    .map_err(|e| Response::error(&req.id, e.code(), e.to_string()))?;

    // `remove_file` unlinks a symlink itself, never its target.
    if let Err(e) = std::fs::remove_file(&path) {
        // A failed remove leaves this file unchanged. Discard its snapshot so
        // the failed request does not become a phantom undo operation.
        ctx.backup()
            .lock()
            .discard_latest_operation_entry_for_path(req.session(), op_id, &path);
        return Err(Response::error(
            &req.id,
            "io_error",
            format!("delete_file: failed to delete: {}", e),
        ));
    }

    ctx.lsp_notify_watched_config_file(path.as_path(), FileChangeType::DELETED);

    log::debug!("delete_file: {}", file);

    let mut result = serde_json::json!({
        "file": file,
        "deleted": true,
    });
    if let Some(ref id) = backup_id {
        result["backup_id"] = serde_json::json!(id);
    }
    if !warnings.is_empty() {
        result["warnings"] = serde_json::json!(warnings);
    }
    edit::attach_backup_skipped_reason(
        &mut result,
        ctx,
        req.session(),
        op_id,
        Some(path.as_path()),
    );
    Ok(result)
}

/// Why nothing at `path` would be backed up, if that holds for the whole
/// entry. A directory is judged by itself; any other entry by the directory
/// that holds it, because judging a symlink by its own path would resolve the
/// link and judge wherever it points instead.
fn no_backup_reason(
    ctx: &AppContext,
    path: &Path,
    is_dir: bool,
) -> Option<crate::backup::BackupSkippedReason> {
    let container = if is_dir {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    ctx.backup().lock().whole_tree_skip_reason(container)
}

/// Delete one non-directory entry that would not be backed up anyway.
fn delete_entry_without_backup(
    req: &RawRequest,
    ctx: &AppContext,
    path: &Path,
    file: &str,
    op_id: &str,
    reason: crate::backup::BackupSkippedReason,
) -> Result<serde_json::Value, Response> {
    // `remove_file` unlinks a symlink itself, never its target.
    std::fs::remove_file(path).map_err(|e| {
        Response::error(
            &req.id,
            "io_error",
            format!("delete_file: failed to delete: {}", e),
        )
    })?;
    ctx.backup()
        .lock()
        .record_skipped_without_snapshot(req.session(), path, op_id, reason);
    ctx.lsp_notify_watched_config_file(path, FileChangeType::DELETED);
    let mut result = serde_json::json!({
        "file": file,
        "deleted": true,
    });
    edit::attach_backup_skipped_reason(&mut result, ctx, req.session(), op_id, Some(path));
    Ok(result)
}

#[cfg(unix)]
fn file_link_count(metadata: &std::fs::Metadata) -> u64 {
    metadata.nlink()
}

#[cfg(not(unix))]
fn file_link_count(_metadata: &std::fs::Metadata) -> u64 {
    1
}

fn detached_hard_link_warning(path: &str) -> String {
    format!(
        "{path}: this file was hard-linked to paths outside this delete; undo restores its content as an independent copy that no longer shares data with them"
    )
}

fn socket_warning(path: &str) -> String {
    format!(
        "{path}: socket deleted and not restorable by undo (a socket holds no data, and one recreated by undo would have no process listening on it)"
    )
}

/// Recursively delete a directory after backing up every entry inside.
///
/// The tree is walked once into a manifest, without following symlinks or
/// entering another filesystem. Every entry is backed up under the same
/// `op_id`, so a single `aft_safety undo` restores the whole tree: directories
/// (including empty ones, with their modes), file contents, hard links
/// (relinked), and symlinks (their exact target text, never the target).
/// Sockets are deleted with a warning that undo does not restore them. Mount
/// points, FIFOs, device nodes and symlinks undo cannot recreate exactly are
/// refused before anything is backed up or deleted.
fn delete_directory(
    req: &RawRequest,
    ctx: &AppContext,
    path: &Path,
    original: &str,
    op_id: &str,
    budget: &mut RecursiveDeleteBackupBudget,
    no_backup: Option<crate::backup::BackupSkippedReason>,
) -> Result<serde_json::Value, Response> {
    // A vanished mounted child can make std::fs::ReadDir::drop panic after
    // closedir returns ENXIO, aborting the daemon. Capture the root device
    // before walking so the walk never crosses it.
    let boundary = crate::walk_boundary::DeviceBoundary::for_root(path).map_err(|e| {
        Response::error(
            &req.id,
            "io_error",
            format!(
                "delete_file: failed to establish filesystem boundary for '{}': {}",
                original, e
            ),
        )
    })?;
    if let Some(reason) = no_backup {
        return delete_directory_without_backups(
            req, ctx, path, original, op_id, &boundary, reason,
        );
    }

    let manifest = match walk_tree(path, &boundary, budget) {
        Ok(manifest) => manifest,
        Err(CollectError::OverBudget(exceeded)) => {
            return Err(over_budget_response(req, original, exceeded, budget));
        }
        Err(CollectError::Io(e)) => {
            return Err(Response::error(
                &req.id,
                "io_error",
                format!(
                    "delete_file: failed to walk directory '{}': {}",
                    original, e
                ),
            ));
        }
    };
    if !manifest.unsupported.is_empty() {
        log_refusal(
            "unsupported_directory_contents",
            path,
            &manifest.unsupported_counts(),
        );
        return Err(Response::error(
            &req.id,
            "unsupported_directory_contents",
            unsupported_contents_message(&manifest),
        ));
    }

    let file_plan = plan_file_backups(&manifest);
    let mut warnings = Vec::new();
    let mut backup_ids: Vec<String> = Vec::new();
    let mut backed_up_paths: Vec<PathBuf> = Vec::new();
    let description = "delete_file: pre-delete backup (directory contents)";
    // Entries are backed up in walk order: each directory before its
    // contents, and each hard link after the path whose content it shares.
    for entry in &manifest.entries {
        let entry_path = entry.path.as_path();
        let display = entry_path.display().to_string();
        let snapshot = match entry.kind {
            NodeKind::Directory => ctx.backup().lock().snapshot_directory_with_op(
                req.session(),
                entry_path,
                description,
                op_id,
            ),
            NodeKind::Symlink => {
                edit::auto_backup(ctx, req.session(), entry_path, description, Some(op_id))
            }
            NodeKind::Socket => {
                warnings.push(socket_warning(&display));
                Ok(None)
            }
            NodeKind::File { .. } => match file_plan.get(&entry.path) {
                Some(FileBackup::LinkTo(first)) => ctx.backup().lock().snapshot_hard_link_with_op(
                    req.session(),
                    entry_path,
                    first,
                    description,
                    op_id,
                ),
                Some(FileBackup::Content { detached: true }) => {
                    warnings.push(detached_hard_link_warning(&display));
                    ctx.backup().lock().snapshot_detached_hard_link_with_op(
                        req.session(),
                        entry_path,
                        description,
                        op_id,
                    )
                }
                _ => edit::auto_backup(ctx, req.session(), entry_path, description, Some(op_id)),
            },
        };
        match snapshot {
            Ok(Some(id)) => {
                backup_ids.push(id);
                backed_up_paths.push(entry.path.clone());
            }
            Ok(None) => {}
            Err(e) => {
                // Nothing has been deleted yet, so snapshots already captured
                // by this failed request must not enter undo history.
                discard_delete_backups(ctx, req.session(), op_id, &backed_up_paths);
                return Err(Response::error(
                    &req.id,
                    e.code(),
                    format!(
                        "delete_file: backup failed for '{}' inside '{}': {}",
                        display, original, e
                    ),
                ));
            }
        }
    }

    #[cfg(debug_assertions)]
    inject_entry_for_tests(path);

    if let Err(stopped) = delete_recorded_tree(&manifest) {
        // Keep backups for entries that are gone, so undo can bring them back,
        // and discard those for entries still present, so undo does not
        // overwrite them with an older copy.
        let not_deleted_paths = backed_up_paths
            .iter()
            .filter(|entry_path| std::fs::symlink_metadata(entry_path).is_ok())
            .cloned()
            .collect::<Vec<_>>();
        discard_delete_backups(ctx, req.session(), op_id, &not_deleted_paths);
        for entry in &manifest.entries {
            if entry.kind != NodeKind::Directory && std::fs::symlink_metadata(&entry.path).is_err()
            {
                ctx.lsp_notify_watched_config_file(&entry.path, FileChangeType::DELETED);
            }
        }
        crate::slog_warn!(
            "delete_file stopped recursive delete of '{}' partway at '{}': {}",
            original,
            stopped.path.display(),
            stopped.reason
        );
        return Err(Response::error_with_data(
            req.id.clone(),
            "io_error",
            format!(
                "delete_file: stopped deleting '{}' partway: could not remove '{}': {}. Entries already removed can be restored with undo; '{}' and whatever still holds it were left in place.",
                original,
                stopped.path.display(),
                stopped.reason,
                stopped.path.display()
            ),
            serde_json::json!({
                "partial": true,
                "stopped_at": stopped.path.display().to_string(),
            }),
        ));
    }

    budget.files_left = budget.files_left.saturating_sub(manifest.entries_counted);
    budget.bytes_left = budget.bytes_left.saturating_sub(manifest.bytes_counted);

    // Notify LSP for every entry that disappeared so watched-file diagnostics
    // refresh.
    let mut files_deleted = 0usize;
    let mut directories_deleted = 0usize;
    for entry in &manifest.entries {
        if entry.kind == NodeKind::Directory {
            directories_deleted += 1;
        } else {
            files_deleted += 1;
            ctx.lsp_notify_watched_config_file(&entry.path, FileChangeType::DELETED);
        }
    }

    log::debug!(
        "delete_file: recursively removed directory '{}' ({} file(s), {} directories)",
        original,
        files_deleted,
        directories_deleted
    );

    let mut result = serde_json::json!({
        "file": original,
        "deleted": true,
        "is_directory": true,
        "files_deleted": files_deleted,
        "directories_deleted": directories_deleted,
        "backup_ids": backup_ids,
    });
    if !warnings.is_empty() {
        result["warnings"] = serde_json::json!(warnings);
    }
    edit::attach_backup_skipped_reason(&mut result, ctx, req.session(), op_id, None);
    Ok(result)
}

/// Test hook for the race between backup and removal: in debug builds, when
/// `AFT_TEST_RECURSIVE_DELETE_INJECT` names a path relative to the tree root,
/// create that file after the backups and before anything is removed.
#[cfg(debug_assertions)]
fn inject_entry_for_tests(root: &Path) {
    if let Some(relative) = std::env::var_os("AFT_TEST_RECURSIVE_DELETE_INJECT") {
        let _ = std::fs::write(root.join(relative), "created during the delete");
    }
}

fn discard_delete_backups(ctx: &AppContext, session: &str, op_id: &str, paths: &[PathBuf]) {
    let mut backup = ctx.backup().lock();
    for path in paths {
        backup.discard_latest_operation_entry_for_path(session, op_id, path);
    }
}

/// Recursively delete a directory none of whose entries would be backed up
/// (it is under a system temp directory, or backups are disabled).
///
/// With no undo to keep whole, symlinks, hard links, empty directories and
/// special files need no refusal and there is no copy to budget. A directory
/// on another filesystem is still refused: removing the tree would descend into
/// it and delete that filesystem's contents.
fn delete_directory_without_backups(
    req: &RawRequest,
    ctx: &AppContext,
    path: &Path,
    original: &str,
    op_id: &str,
    boundary: &crate::walk_boundary::DeviceBoundary,
    reason: crate::backup::BackupSkippedReason,
) -> Result<serde_json::Value, Response> {
    let mut files = Vec::new();
    let mut mounts = Vec::new();
    collect_for_unbacked_delete(path, boundary, &mut files, &mut mounts).map_err(|e| {
        Response::error(
            &req.id,
            "io_error",
            format!(
                "delete_file: failed to walk directory '{}': {}",
                original, e
            ),
        )
    })?;
    if !mounts.is_empty() {
        log_refusal(
            "unsupported_directory_contents",
            path,
            &[(UnsupportedKind::OtherFilesystem, mounts.len())],
        );
        return Err(Response::error(
            &req.id,
            "unsupported_directory_contents",
            other_filesystem_message(&mounts),
        ));
    }

    // Rust's remove_dir_all unlinks symlinks without following them.
    std::fs::remove_dir_all(path).map_err(|e| {
        Response::error(
            &req.id,
            "io_error",
            format!(
                "delete_file: failed to remove directory '{}': {}",
                original, e
            ),
        )
    })?;
    ctx.backup()
        .lock()
        .record_skipped_without_snapshot(req.session(), path, op_id, reason);
    for file_path in &files {
        ctx.lsp_notify_watched_config_file(file_path.as_path(), FileChangeType::DELETED);
    }

    let mut result = serde_json::json!({
        "file": original,
        "deleted": true,
        "is_directory": true,
        "files_deleted": files.len(),
        "backup_ids": Vec::<String>::new(),
    });
    edit::attach_backup_skipped_reason(&mut result, ctx, req.session(), op_id, None);
    Ok(result)
}

/// Collect every non-directory entry (for change notifications) and every
/// directory on another filesystem, without following symlinks.
fn collect_for_unbacked_delete(
    dir: &Path,
    boundary: &crate::walk_boundary::DeviceBoundary,
    files: &mut Vec<PathBuf>,
    mounts: &mut Vec<String>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // `DirEntry::file_type` does not follow symlinks.
        if entry.file_type()?.is_dir() {
            if boundary.should_descend(&path)? {
                collect_for_unbacked_delete(&path, boundary, files, mounts)?;
            } else {
                mounts.push(path.display().to_string());
            }
        } else {
            files.push(path);
        }
    }
    Ok(())
}

fn other_filesystem_message(paths: &[String]) -> String {
    let mut message = String::from(
        "aft_delete refuses to delete a directory tree that contains a mount point of another filesystem: removing the tree would delete that filesystem's contents. Unmount it first.",
    );
    append_offending_paths(&mut message, paths);
    message
}

fn append_offending_paths(message: &mut String, paths: &[String]) {
    const MAX_PATHS: usize = 5;
    message.push_str(" Offending path(s): ");
    message.push_str(
        &paths
            .iter()
            .take(MAX_PATHS)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", "),
    );
    if paths.len() > MAX_PATHS {
        message.push_str(&format!(", ... and {} more", paths.len() - MAX_PATHS));
    }
}

fn unsupported_contents_message(manifest: &TreeManifest) -> String {
    let mut kinds = manifest
        .unsupported_counts()
        .into_iter()
        .map(|(kind, _)| unsupported_kind_reason(kind))
        .collect::<Vec<_>>();
    kinds.dedup();
    let mut message = format!(
        "aft_delete with recursive: true refuses this directory tree because it contains entries undo cannot restore or that removing would reach beyond the tree: {}. Nothing was deleted.",
        kinds.join("; ")
    );
    let paths = manifest
        .unsupported
        .iter()
        .map(|(path, _)| path.display().to_string())
        .collect::<Vec<_>>();
    append_offending_paths(&mut message, &paths);
    message
}

/// Plain-language reason a kind of entry is refused.
fn unsupported_kind_reason(kind: UnsupportedKind) -> &'static str {
    match kind {
        UnsupportedKind::OtherFilesystem => {
            "a mount point of another filesystem (removing it would delete that filesystem's contents; unmount it first)"
        }
        UnsupportedKind::Fifo => "a named pipe (FIFO), which undo does not recreate",
        UnsupportedKind::Device => {
            "a device node, which undo could not recreate without special privileges"
        }
        UnsupportedKind::SymlinkNonUtf8Target | UnsupportedKind::WindowsSymlink => {
            "a symlink undo cannot recreate exactly"
        }
        UnsupportedKind::Other => "a special file undo cannot recreate",
    }
}

fn unsupported_symlink_reason(kind: UnsupportedKind) -> &'static str {
    match kind {
        UnsupportedKind::SymlinkNonUtf8Target => {
            "its target is not valid UTF-8, and undo could not recreate it exactly"
        }
        UnsupportedKind::WindowsSymlink => {
            "undo cannot yet recreate symlinks on Windows with their file or directory type"
        }
        other => unsupported_kind_reason(other),
    }
}

/// Log a refusal with its code and how many offending entries of each kind
/// it found, so refusals can be counted from the daemon log.
fn log_refusal(code: &str, target: &Path, counts: &[(UnsupportedKind, usize)]) {
    let counts = counts
        .iter()
        .map(|(kind, count)| format!("{}={}", kind.as_str(), count))
        .collect::<Vec<_>>()
        .join(" ");
    crate::slog_warn!(
        "delete_file refused '{}': code={} offending: {}",
        target.display(),
        code,
        counts
    );
}

fn budget_for_request(ctx: &AppContext) -> RecursiveDeleteBackupBudget {
    RecursiveDeleteBackupBudget::new(
        ctx.backup().lock().policy(),
        crate::backup::RECURSIVE_DELETE_BACKUP_MAX_FILES,
        crate::backup::RECURSIVE_DELETE_BACKUP_MAX_BYTES,
    )
}
fn format_mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn over_budget_response(
    req: &RawRequest,
    original: &str,
    exceeded: BudgetExceeded,
    budget: &RecursiveDeleteBackupBudget,
) -> Response {
    let max_files = crate::backup::RECURSIVE_DELETE_BACKUP_MAX_FILES;
    let max_bytes = crate::backup::RECURSIVE_DELETE_BACKUP_MAX_BYTES;
    // Earlier directories in the same batch call already spent part of the
    // budget; say so, or the numbers below would not add up for the caller.
    let used_files = max_files.saturating_sub(budget.files_left);
    let used_bytes = max_bytes.saturating_sub(budget.bytes_left);
    let counted = match exceeded.limit {
        BudgetLimit::Files => format!("at least {} entries", exceeded.files_counted),
        BudgetLimit::Bytes => format!(
            "at least {} in {} entries",
            format_mib(exceeded.bytes_counted),
            exceeded.files_counted
        ),
    };
    let earlier = if used_files > 0 || used_bytes > 0 {
        format!(
            " Earlier directories in this call already used {} entries and {}.",
            used_files,
            format_mib(used_bytes)
        )
    } else {
        String::new()
    };
    crate::slog_warn!(
        "delete_file refused recursive delete of '{}': code=recursive_delete_backup_too_large limit={} entries_counted_at_least={} bytes_counted_at_least={}",
        original,
        match exceeded.limit {
            BudgetLimit::Files => "entries",
            BudgetLimit::Bytes => "bytes",
        },
        exceeded.files_counted,
        exceeded.bytes_counted
    );
    Response::error_with_data(
        req.id.clone(),
        "recursive_delete_backup_too_large",
        format!(
            "delete_file: refusing to delete '{original}': its undo backup would record {counted} \
             (limit per call: {max_files} entries, counting files, directories and links, and {}); \
             counting stopped at the limit.{earlier} \
             Nothing was deleted. Delete it in smaller pieces to keep undo, or, when no undo \
             is needed, remove it with bash `rm -rf`.",
            format_mib(max_bytes)
        ),
        serde_json::json!({
            "limit": match exceeded.limit {
                BudgetLimit::Files => "files",
                BudgetLimit::Bytes => "bytes",
            },
            "files_counted_at_least": exceeded.files_counted,
            "bytes_counted_at_least": exceeded.bytes_counted,
            "max_files": max_files,
            "max_bytes": max_bytes,
            "counting_stopped_early": true,
        }),
    )
}
