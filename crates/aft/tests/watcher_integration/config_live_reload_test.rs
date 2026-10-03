//! Live reload of config file edits through the real project watcher.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::helpers::AftProcess;

fn configure(aft: &mut AftProcess, root: &Path, user_config: &Path) {
    let response = aft.send(
        &json!({
            "id": "cfg-live-reload",
            "command": "configure",
            "harness": "opencode",
            "project_root": root,
            "cortexkit_user_config_path": user_config,
        })
        .to_string(),
    );
    assert_eq!(response["success"], true, "configure failed: {response:?}");
}

fn bash_true(aft: &mut AftProcess) -> Value {
    aft.send(
        &json!({
            "id": "live-reload-bash",
            "command": "bash",
            "params": { "command": "true" }
        })
        .to_string(),
    )
}

fn wait_for_bash_disabled(aft: &mut AftProcess) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = bash_true(aft);
        if last["code"] == "bash_disabled" {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("the config edit was never applied; last bash response: {last:?}");
}

fn live_edit_disables_bash(gitignore: Option<&str>) {
    let _watcher_guard = crate::helpers::watcher_serial_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(root.join(".cortexkit")).unwrap();
    if let Some(gitignore) = gitignore {
        fs::write(root.join(".gitignore"), gitignore).unwrap();
    }
    let project_config = root.join(".cortexkit/aft.jsonc");
    fs::write(&project_config, r#"{ "bash": { "enabled": true } }"#).unwrap();
    let user_config = dir.path().join("xdg/cortexkit/aft.jsonc");
    fs::create_dir_all(user_config.parent().unwrap()).unwrap();
    fs::write(&user_config, "{}").unwrap();

    let mut aft = AftProcess::spawn_with_real_watcher();
    configure(&mut aft, &root, &user_config);
    assert_eq!(bash_true(&mut aft)["success"], true);

    // Edit the file and send no configure: the next requests see the change.
    thread::sleep(Duration::from_millis(300));
    fs::write(&project_config, r#"{ "bash": { "enabled": false } }"#).unwrap();
    wait_for_bash_disabled(&mut aft);
}

#[test]
fn project_config_edit_applies_live_through_the_project_watcher() {
    live_edit_disables_bash(None);
}

#[test]
fn project_config_edit_applies_live_when_cortexkit_is_gitignored() {
    live_edit_disables_bash(Some(".cortexkit/\n"));
}

#[test]
fn user_config_edit_applies_live() {
    let _watcher_guard = crate::helpers::watcher_serial_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(&root).unwrap();
    let user_config = dir.path().join("xdg/cortexkit/aft.jsonc");
    fs::create_dir_all(user_config.parent().unwrap()).unwrap();
    fs::write(&user_config, "{}").unwrap();

    let mut aft = AftProcess::spawn_with_real_watcher();
    configure(&mut aft, &root, &user_config);
    assert_eq!(bash_true(&mut aft)["success"], true);

    thread::sleep(Duration::from_millis(300));
    fs::write(&user_config, r#"{ "bash": { "enabled": false } }"#).unwrap();
    wait_for_bash_disabled(&mut aft);
}
