//! The plugins attach `worker_session: true` next to `session_id` on every
//! request from a delegated worker (subagent) session. A worker cannot be
//! woken once its turn ends, so the engine must never promise it a completion
//! reminder. These tests send the flag the way the plugins do over the
//! standalone protocol and check the bash hand-off text each role gets. (The
//! role on a `tool_call` envelope is covered by the repeat-breaker tests: a
//! standalone `tool_call` bash returns the bare spawn reply, without a
//! hand-off text.)

use std::time::Duration;

use serde_json::{json, Value};

use crate::test_helpers::AftProcess;

const SESSION: &str = "worker-session-test";

fn background_bash_params(project: &std::path::Path) -> Value {
    json!({
        "command": "sleep 5",
        "workdir": project,
        "background": true,
        "notify_on_completion": true,
        "foreground_orchestrate": true,
    })
}

/// A raw `bash` request, as the Pi plugin and the OpenCode plugin in
/// standalone mode send it: the arguments nested under `params`, the role at
/// the top level beside `session_id`.
fn raw_bash_launch_text(aft: &mut AftProcess, project: &std::path::Path, worker: bool) -> String {
    let mut request = json!({
        "id": format!("raw-bash-worker-{worker}"),
        "command": "bash",
        "session_id": SESSION,
        "params": background_bash_params(project),
    });
    if worker {
        request["worker_session"] = json!(true);
    }
    let response = aft.send_with_timeout(&request.to_string(), Duration::from_secs(10));
    assert_eq!(response["success"], true, "raw bash: {response:?}");
    let task_id = response["task_id"].as_str().unwrap_or_default().to_string();
    kill(aft, &task_id);
    response["output"].as_str().unwrap_or_default().to_string()
}

fn kill(aft: &mut AftProcess, task_id: &str) {
    if task_id.is_empty() {
        return;
    }
    let _ = aft.send_with_timeout(
        &json!({
            "id": format!("kill-{task_id}"),
            "command": "bash_kill",
            "session_id": SESSION,
            "task_id": task_id,
        })
        .to_string(),
        Duration::from_secs(10),
    );
}

fn assert_worker_hand_off(label: &str, text: &str) {
    assert!(
        text.contains("Background task started") && text.contains("won't wake you"),
        "{label}: a worker must be told the task won't wake it: {text:?}"
    );
    assert!(
        !text.contains("completion reminder"),
        "{label}: a worker must not be promised a completion reminder: {text:?}"
    );
}

fn assert_primary_hand_off(label: &str, text: &str) {
    assert!(
        text.contains("A completion reminder will be delivered automatically"),
        "{label}: a primary keeps the completion-reminder text: {text:?}"
    );
}

#[test]
fn worker_role_reaches_the_bash_hand_off_text_over_standalone() {
    let project = tempfile::tempdir().expect("worker session project");
    let mut aft = AftProcess::spawn();
    aft.configure(project.path());

    assert_worker_hand_off(
        "raw bash",
        &raw_bash_launch_text(&mut aft, project.path(), true),
    );
    assert_primary_hand_off(
        "raw bash",
        &raw_bash_launch_text(&mut aft, project.path(), false),
    );
    assert!(aft.shutdown().success());
}
