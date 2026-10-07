#![cfg(unix)]

use super::helpers::{user_config, AftProcess};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn configure(aft: &mut AftProcess, dir: &tempfile::TempDir, enabled: bool) {
    let created = std::process::Command::new("sqlite3")
        .arg(dir.path().join("store.db"))
        .arg("CREATE TABLE tasks(id TEXT, kind TEXT);")
        .output()
        .expect("sqlite3 must be installed");
    assert!(created.status.success());
    let response = aft.send(
        &json!({
            "id": "db-cfg", "command": "configure", "harness": "opencode",
            "project_root": dir.path(), "storage_dir": dir.path().join("storage"),
            "config": user_config(json!({"bash": {"db_schema_hints": enabled}}))
        })
        .to_string(),
    );
    assert_eq!(response["success"], true, "{response}");
}

fn terminal(aft: &mut AftProcess, task: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = aft.send(
            &json!({"id":"db-status", "command":"bash_status", "params":{"task_id":task}})
                .to_string(),
        );
        if response["status"] != "running" {
            return response;
        }
        assert!(Instant::now() < deadline, "{response}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn db_hints_foreground_trailer_independent_of_compression_and_switch() {
    for enabled in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut aft = AftProcess::spawn();
        configure(&mut aft, &dir, enabled);
        for compressed in [true, false] {
            let response = aft.send(
                &json!({"id":"db-fg", "method":"bash", "params": {
                    "command":"sqlite3 store.db 'SELECT substrate FROM tasks'",
                    "foreground_orchestrate":true, "compressed":compressed,
                }})
                .to_string(),
            );
            assert_eq!(response["status"], "failed", "{response}");
            let output = response["output"].as_str().unwrap();
            assert_eq!(
                output.matches("[aft: no column").count(),
                usize::from(enabled),
                "{output}"
            );
            if enabled {
                assert!(output.contains("tasks: id TEXT, kind TEXT"), "{output}");
            }
        }
        assert!(aft.shutdown().success());
    }
}

#[test]
fn db_hints_background_completion_keeps_trailer_after_preview_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &dir, true);
    let launched = aft.send(
        &json!({"id":"db-bg", "method":"bash", "params": {
            "command":"sqlite3 store.db 'SELECT hex(randomblob(70000)); SELECT missing FROM tasks'",
            "background":true, "compressed":false,
        }})
        .to_string(),
    );
    assert_eq!(launched["status"], "running", "{launched}");
    assert!(!launched.to_string().contains("[aft: no column"));
    let task = launched["task_id"].as_str().unwrap();
    let response = terminal(&mut aft, task);
    assert_eq!(response["status"], "failed", "{response}");
    let preview = response["output_preview"].as_str().unwrap();
    assert_eq!(preview.matches("[aft: no column").count(), 1, "{preview}");
    let drained =
        aft.send(&json!({"id":"db-drain", "command":"bash_drain_completions"}).to_string());
    let completions = drained["bg_completions"].as_array().unwrap();
    let completion = completions
        .iter()
        .find(|c| c["task_id"] == task)
        .expect("background completion");
    let preview = completion["output_preview"].as_str().unwrap();
    assert!(preview.contains("tasks: id TEXT, kind TEXT"), "{preview}");
    assert_eq!(preview.matches("[aft: no column").count(), 1, "{preview}");
    assert!(aft.shutdown().success());
}

#[test]
fn db_hints_terminal_pty_status_and_completion() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &dir, true);
    let launched = aft.send(
        &json!({"id":"db-pty", "method":"bash", "params": {
            "command":"sqlite3 store.db 'SELECT missing FROM tasks'", "pty":true,
        }})
        .to_string(),
    );
    assert_eq!(launched["status"], "running", "{launched}");
    let task = launched["task_id"].as_str().unwrap();
    let response = terminal(&mut aft, task);
    assert_eq!(response["status"], "failed", "{response}");
    let preview = response["output_preview"].as_str().unwrap();
    assert!(preview.contains("tasks: id TEXT, kind TEXT"), "{preview}");
    let drained =
        aft.send(&json!({"id":"db-drain-pty", "command":"bash_drain_completions"}).to_string());
    let completion = drained["bg_completions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["task_id"] == task)
        .unwrap();
    assert!(
        completion["output_preview"]
            .as_str()
            .unwrap()
            .contains("tasks: id TEXT, kind TEXT"),
        "{completion}"
    );
    assert!(aft.shutdown().success());
}
