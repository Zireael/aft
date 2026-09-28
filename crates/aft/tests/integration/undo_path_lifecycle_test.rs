//! Undo and history for one path across the file's deletion, recreation, and
//! replacement.
//!
//! The undo store is keyed by path, so a path's stack can span several files
//! that lived there one after another. These tests drive the real binary
//! through each way a file can be removed or replaced (AFT's delete, `rm`,
//! AFT's move into the path, `mv` over the file, an editor's temp-file save)
//! and check two promises: undo never destroys content AFT did not write, and
//! `edit_history` marks where one file's life at the path ended.

use super::helpers::AftProcess;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn send(aft: &mut AftProcess, request: Value) -> Value {
    aft.send(&serde_json::to_string(&request).unwrap())
}

fn write(aft: &mut AftProcess, file: &Path, content: &str) {
    let response = send(
        aft,
        json!({"id": "write", "command": "write", "file": file.display().to_string(), "content": content}),
    );
    assert_eq!(response["success"], true, "write: {response:?}");
}

fn edit(aft: &mut AftProcess, file: &Path, find: &str, replacement: &str) {
    let response = send(
        aft,
        json!({
            "id": "edit",
            "command": "edit_match",
            "file": file.display().to_string(),
            "match": find,
            "replacement": replacement,
        }),
    );
    assert_eq!(response["success"], true, "edit: {response:?}");
}

fn undo_file(aft: &mut AftProcess, file: &Path) -> Value {
    let response = send(
        aft,
        json!({"id": "undo", "command": "undo", "file": file.display().to_string()}),
    );
    assert_eq!(response["success"], true, "undo: {response:?}");
    response
}

fn undo_operation(aft: &mut AftProcess) -> Value {
    let response = send(aft, json!({"id": "undo-op", "command": "undo"}));
    assert_eq!(response["success"], true, "operation undo: {response:?}");
    response
}

/// History entries newest first.
fn history(aft: &mut AftProcess, file: &Path) -> Vec<Value> {
    let response = send(
        aft,
        json!({"id": "history", "command": "edit_history", "file": file.display().to_string()}),
    );
    assert_eq!(response["success"], true, "history: {response:?}");
    response["entries"].as_array().unwrap().clone()
}

fn markers(entry: &Value) -> Vec<&str> {
    entry["markers"]
        .as_array()
        .unwrap_or_else(|| panic!("history entry has no markers: {entry:?}"))
        .iter()
        .map(|marker| marker.as_str().unwrap())
        .collect()
}

fn read(file: &Path) -> Option<String> {
    fs::read_to_string(file).ok()
}

/// Write A0, edit it to A1 then A2: three snapshots for the first file.
fn first_file_with_history(aft: &mut AftProcess, file: &Path) {
    write(aft, file, "A0\n");
    edit(aft, file, "A0", "A1");
    edit(aft, file, "A1", "A2");
}

/// Assert the history belongs to two files: the newest `newer` entries to the
/// current one, the rest labelled as a previous file.
fn assert_two_files(entries: &[Value], newer: usize) {
    for (index, entry) in entries.iter().enumerate() {
        let previous = index >= newer;
        assert_eq!(
            entry["previous_file"], previous,
            "entry {index} previous_file: {entries:#?}"
        );
        assert_eq!(
            entry["generation"],
            if previous { 0 } else { 1 },
            "entry {index} generation: {entries:#?}"
        );
    }
}

#[test]
fn aft_delete_then_recreate_labels_previous_file_and_undo_walks_the_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let mut aft = AftProcess::spawn();
    first_file_with_history(&mut aft, &file);
    let delete = send(
        &mut aft,
        json!({"id": "delete", "command": "delete_file", "files": [file.display().to_string()]}),
    );
    assert_eq!(delete["success"], true, "delete: {delete:?}");
    write(&mut aft, &file, "B0\n");
    edit(&mut aft, &file, "B0", "B1");

    let entries = history(&mut aft, &file);
    assert_eq!(entries.len(), 6, "{entries:#?}");
    assert_two_files(&entries, 2);
    assert!(markers(&entries[1]).contains(&"created"), "{entries:#?}");
    assert!(markers(&entries[2]).contains(&"deleted"), "{entries:#?}");

    // Every step reverses one change AFT itself made, so none needs to
    // preserve anything, and undoing the delete still brings A back.
    let mut states = Vec::new();
    for _ in 0..6 {
        let undo = undo_file(&mut aft, &file);
        assert!(
            undo.get("warning").is_none(),
            "unexpected warning: {undo:?}"
        );
        assert!(undo.get("preserved_external_change").is_none(), "{undo:?}");
        states.push(read(&file));
    }
    let expected = [
        Some("B0\n"),
        None,
        Some("A2\n"),
        Some("A1\n"),
        Some("A0\n"),
        None,
    ];
    assert_eq!(
        states,
        expected.map(|state| state.map(str::to_string)).to_vec()
    );
    assert!(aft.shutdown().success());
}

#[test]
fn rm_then_recreate_labels_previous_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let mut aft = AftProcess::spawn();
    first_file_with_history(&mut aft, &file);
    fs::remove_file(&file).unwrap();
    write(&mut aft, &file, "B0\n");
    edit(&mut aft, &file, "B0", "B1");

    let entries = history(&mut aft, &file);
    assert_eq!(entries.len(), 5, "{entries:#?}");
    assert_two_files(&entries, 2);
    let creation = markers(&entries[1]);
    assert!(creation.contains(&"created"), "{entries:#?}");
    assert!(
        creation.contains(&"changed_outside_aft_before"),
        "the rm AFT never saw must be marked: {entries:#?}"
    );

    assert_eq!(read(&file).as_deref(), Some("B1\n"));
    undo_file(&mut aft, &file);
    assert_eq!(read(&file).as_deref(), Some("B0\n"));
    undo_file(&mut aft, &file);
    assert_eq!(read(&file), None);
    // The path is empty, so restoring the previous file's content loses nothing.
    let undo = undo_file(&mut aft, &file);
    assert!(undo.get("preserved_external_change").is_none(), "{undo:?}");
    assert_eq!(read(&file).as_deref(), Some("A1\n"));
    assert!(aft.shutdown().success());
}

#[test]
fn aft_move_into_vacated_path_labels_previous_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let other = dir.path().join("b.txt");
    let mut aft = AftProcess::spawn();
    first_file_with_history(&mut aft, &file);
    // move_file refuses an existing destination, so the path is vacated first.
    fs::remove_file(&file).unwrap();
    fs::write(&other, "B0\n").unwrap();
    let moved = send(
        &mut aft,
        json!({
            "id": "move",
            "command": "move_file",
            "file": other.display().to_string(),
            "destination": file.display().to_string(),
        }),
    );
    assert_eq!(moved["success"], true, "move: {moved:?}");
    edit(&mut aft, &file, "B0", "B1");

    let entries = history(&mut aft, &file);
    assert_eq!(entries.len(), 5, "{entries:#?}");
    assert_two_files(&entries, 2);
    assert!(markers(&entries[1]).contains(&"created"), "{entries:#?}");
    assert!(aft.shutdown().success());
}

/// `mv b.txt a.txt` over a file AFT has edited, then one AFT edit of the new
/// content. Returns with a.txt holding B1.
fn rename_over_file_with_history(aft: &mut AftProcess, dir: &Path) -> std::path::PathBuf {
    let file = dir.join("a.txt");
    let other = dir.join("b.txt");
    first_file_with_history(aft, &file);
    fs::write(&other, "B0\n").unwrap();
    fs::rename(&other, &file).unwrap();
    edit(aft, &file, "B0", "B1");
    file
}

#[test]
fn rename_over_file_undo_preserves_the_new_file_and_undo_again_restores_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();
    let file = rename_over_file_with_history(&mut aft, dir.path());

    let entries = history(&mut aft, &file);
    assert_eq!(
        markers(&entries[0]),
        vec!["changed_outside_aft_before"],
        "{entries:#?}"
    );

    let first = undo_file(&mut aft, &file);
    assert!(first.get("warning").is_none(), "{first:?}");
    assert_eq!(read(&file).as_deref(), Some("B0\n"));

    // The next entry was A's last edit, but the path now holds B0, which AFT
    // never wrote there. Undo restores A1 only after saving B0.
    let second = undo_file(&mut aft, &file);
    assert_eq!(read(&file).as_deref(), Some("A1\n"));
    let preserved = second["preserved_external_change"]
        .as_str()
        .unwrap_or_else(|| panic!("B0 was not preserved: {second:?}"))
        .to_string();
    assert!(
        second["warning"]
            .as_str()
            .is_some_and(|warning| warning.contains(&preserved)),
        "{second:?}"
    );
    let entries = history(&mut aft, &file);
    assert_eq!(entries[0]["backup_id"], preserved.as_str(), "{entries:#?}");
    assert!(
        markers(&entries[0]).contains(&"captured_before_undo"),
        "{entries:#?}"
    );

    let third = undo_file(&mut aft, &file);
    assert_eq!(third["backup_id"], preserved.as_str(), "{third:?}");
    assert_eq!(
        read(&file).as_deref(),
        Some("B0\n"),
        "B0 must be recoverable"
    );
    assert!(aft.shutdown().success());
}

#[test]
fn rename_over_file_operation_undo_preserves_the_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();
    let file = rename_over_file_with_history(&mut aft, dir.path());

    undo_operation(&mut aft);
    assert_eq!(read(&file).as_deref(), Some("B0\n"));
    let second = undo_operation(&mut aft);
    assert_eq!(read(&file).as_deref(), Some("A1\n"));
    assert!(
        second["restored"][0]["preserved_external_change"].is_string(),
        "{second:?}"
    );
    assert_eq!(
        second["warnings"].as_array().unwrap().len(),
        1,
        "{second:?}"
    );

    undo_operation(&mut aft);
    assert_eq!(
        read(&file).as_deref(),
        Some("B0\n"),
        "B0 must be recoverable"
    );
    assert!(aft.shutdown().success());
}

/// Save `content` the way many editors do: write a sibling temp file and
/// rename it over the original, which gives the path a new inode.
fn editor_atomic_save(file: &Path, content: &str) {
    let temp = file.with_extension("txt.swp");
    fs::write(&temp, content).unwrap();
    fs::rename(&temp, file).unwrap();
}

#[test]
fn editor_atomic_save_with_unchanged_content_undoes_normally() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let mut aft = AftProcess::spawn();
    first_file_with_history(&mut aft, &file);
    editor_atomic_save(&file, "A2\n");

    let undo = undo_file(&mut aft, &file);
    assert!(undo.get("warning").is_none(), "{undo:?}");
    assert!(undo.get("preserved_external_change").is_none(), "{undo:?}");
    assert_eq!(read(&file).as_deref(), Some("A1\n"));
    assert_eq!(history(&mut aft, &file).len(), 2);
    assert!(aft.shutdown().success());
}

#[test]
fn editor_atomic_save_with_new_content_is_preserved_before_undo() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let mut aft = AftProcess::spawn();
    first_file_with_history(&mut aft, &file);
    editor_atomic_save(&file, "A2 plus editor work\n");

    let undo = undo_file(&mut aft, &file);
    assert!(undo["preserved_external_change"].is_string(), "{undo:?}");
    assert_eq!(read(&file).as_deref(), Some("A1\n"));
    undo_file(&mut aft, &file);
    assert_eq!(read(&file).as_deref(), Some("A2 plus editor work\n"));
    assert!(aft.shutdown().success());
}

#[test]
fn undoing_aft_own_edits_reports_no_external_change() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let mut aft = AftProcess::spawn();
    first_file_with_history(&mut aft, &file);

    for expected in [Some("A1\n"), Some("A0\n"), None] {
        let undo = undo_file(&mut aft, &file);
        assert!(
            undo.get("warning").is_none(),
            "undo of AFT's own edit must not claim an external change: {undo:?}"
        );
        assert!(undo.get("preserved_external_change").is_none(), "{undo:?}");
        assert_eq!(read(&file).as_deref(), expected);
    }
    assert!(aft.shutdown().success());
}
