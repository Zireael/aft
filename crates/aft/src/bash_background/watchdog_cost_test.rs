use super::super::persistence::{allocate_task_layout, work_counts, write_task_at};
use super::*;

fn fixture(
    storage: &Path,
    session: &str,
    delivered: bool,
    days: u64,
) -> (PersistedTask, TaskPaths) {
    let resolved = allocate_task_layout(storage, session).unwrap();
    let mut metadata = PersistedTask::starting(
        resolved.paths.task_id.clone(),
        session.into(),
        "watchdog-fixture".into(),
        storage.join("deleted-workdir"),
        Some(storage.join("deleted-root")),
        None,
        true,
        false,
    );
    metadata.mark_terminal(BgTaskStatus::Completed, Some(0), None);
    metadata.completion_delivered = delivered;
    metadata.finished_at = Some(unix_millis() - days * 24 * 60 * 60 * 1000);
    if days > 0 {
        metadata.started_at = metadata.finished_at.unwrap().saturating_sub(60_000);
        metadata.duration_ms = Some(60_000);
    }
    write_task_at(&resolved, &metadata).unwrap();
    for artifact in [
        &resolved.paths.stdout,
        &resolved.paths.stderr,
        &resolved.paths.exit,
    ] {
        fs::write(artifact, b"").unwrap();
    }
    filetime::set_file_mtime(
        &resolved.paths.json,
        filetime::FileTime::from_unix_time((unix_millis() / 1000) as i64 - 25 * 60 * 60, 0),
    )
    .unwrap();
    (metadata, resolved.paths)
}

#[cfg(unix)]
#[test]
fn watchdog_cost_live_children_open_no_artifacts() {
    let storage = tempfile::tempdir().unwrap();
    let registries: Vec<_> = (0..30).map(|_| BgTaskRegistry::default()).collect();
    // Real directory-layout history, not a proxy counter for the task map.
    for index in 0..10_000 {
        let (metadata, paths) = fixture(storage.path(), &format!("root-{}", index % 30), true, 2);
        registries[index % 30]
            .insert_rehydrated_task(metadata, paths, true)
            .unwrap();
    }
    let mut tasks = Vec::new();
    for index in 0..40 {
        let child = std::process::Command::new("/bin/sleep")
            .arg("300")
            .spawn()
            .unwrap();
        let (mut metadata, paths) =
            fixture(storage.path(), &format!("root-{}", index % 30), false, 0);
        metadata.mark_running(child.id(), child.id() as i32);
        metadata.finished_at = None;
        metadata.duration_ms = None;
        write_task_at(
            &resolve_task_layout(&paths.session_dir, &paths.task_id).unwrap(),
            &metadata,
        )
        .unwrap();
        let registry = &registries[index % 30];
        registry
            .insert_rehydrated_task(metadata, paths.clone(), false)
            .unwrap();
        let task = registry.task(&paths.task_id).unwrap();
        std::mem::swap(
            &mut task.state.lock().unwrap().runtime,
            &mut TaskRuntime::Piped(Some(child)),
        );
        tasks.push((index % 30, task));
    }
    work_counts::reset();
    for (root, task) in &tasks {
        registries[*root].poll_task(task).unwrap();
        registries[*root].reap_child(task);
    }
    let counts = work_counts::get();
    work_counts::reset();
    for registry in &registries {
        registry.cleanup_finished(Duration::from_secs(3600));
    }
    let cleanup = work_counts::get();
    let finished_remaining: usize = registries
        .iter()
        .map(|registry| registry.inner.tasks.lock().unwrap().len())
        .sum::<usize>()
        - 40;
    eprintln!(
        "one minute cleanup / 30 registries: {cleanup:?}; finished deleted={}",
        10_000 - finished_remaining
    );
    work_counts::reset();
    let gc_deleted = registries[0].maybe_gc_persisted(storage.path()).unwrap();
    let persisted_cleanup = work_counts::get();
    eprintln!("one minute shared persisted GC: {persisted_cleanup:?}; deleted={gc_deleted}");
    eprintln!("40 children / 30 registries / 10000 finished folders: per tick {counts:?}; per minute opens={} reads={} stats={}", counts.opens * 120, (counts.metadata_reads + counts.marker_reads) * 120, counts.stats * 120);
    for (_, task) in tasks {
        if let TaskRuntime::Piped(Some(mut child)) = std::mem::replace(
            &mut task.state.lock().unwrap().runtime,
            TaskRuntime::Piped(None),
        ) {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
    assert_eq!(
        counts.opens, 0,
        "live child ticks reopened persisted artifacts"
    );
}

#[test]
fn watchdog_cost_cleanup_is_bounded_and_makes_progress() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    for _ in 0..300 {
        let (metadata, paths) = fixture(storage.path(), "session", true, 2);
        registry
            .insert_rehydrated_task(metadata, paths.clone(), true)
            .unwrap();
        *registry
            .task(&paths.task_id)
            .unwrap()
            .terminal_at
            .lock()
            .unwrap() = Some(Instant::now() - Duration::from_secs(7200));
    }
    registry.cleanup_finished(Duration::from_secs(3600));
    let remaining = registry.inner.tasks.lock().unwrap().len();
    assert!(
        remaining >= 172,
        "one cleanup scanned/deleted all history: {remaining} remain"
    );
    for _ in 0..6 {
        registry.cleanup_finished(Duration::from_secs(3600));
    }
    assert!(
        registry.inner.tasks.lock().unwrap().is_empty(),
        "cursor starved later entries"
    );
}

#[test]
fn watchdog_cost_persisted_gc_is_bounded_at_directory_iterator() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    let mut paths = Vec::new();
    for _ in 0..600 {
        paths.push(fixture(storage.path(), "session", true, 2).1);
    }
    work_counts::reset();
    registry.maybe_gc_persisted(storage.path()).unwrap();
    let counts = work_counts::get();
    eprintln!("persisted GC first pass: {counts:?}");
    assert!(
        counts.gc_entries <= 128,
        "GC eagerly advanced the directory iterator: {} entries",
        counts.gc_entries
    );
    let remain = paths.iter().filter(|path| path.json.exists()).count();
    assert!(
        remain >= 472,
        "GC exhausted the directory before applying its budget: {remain}"
    );
    for _ in 0..12 {
        registry.maybe_gc_persisted(storage.path()).unwrap();
    }
    assert!(paths.iter().all(|path| !path.json.exists()));
}

#[test]
fn watchdog_cost_abandonment_requires_both_paths_missing_and_seven_days() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    let (_, abandoned) = fixture(storage.path(), "session", false, 8);
    let (_, recent) = fixture(storage.path(), "session", false, 6);
    let (mut metadata, workdir_live) = fixture(storage.path(), "session", false, 8);
    metadata.workdir = storage.path().to_path_buf();
    write_task_at(
        &resolve_task_layout(&workdir_live.session_dir, &workdir_live.task_id).unwrap(),
        &metadata,
    )
    .unwrap();
    filetime::set_file_mtime(
        &workdir_live.json,
        filetime::FileTime::from_unix_time((unix_millis() / 1000) as i64 - 25 * 3600, 0),
    )
    .unwrap();
    let (mut metadata, root_live) = fixture(storage.path(), "session", false, 8);
    metadata.project_root = Some(storage.path().to_path_buf());
    write_task_at(
        &resolve_task_layout(&root_live.session_dir, &root_live.task_id).unwrap(),
        &metadata,
    )
    .unwrap();
    filetime::set_file_mtime(
        &root_live.json,
        filetime::FileTime::from_unix_time((unix_millis() / 1000) as i64 - 25 * 3600, 0),
    )
    .unwrap();
    registry.maybe_gc_persisted(storage.path()).unwrap();
    assert!(
        !abandoned.json.exists(),
        "deleted-worktree completion retained forever"
    );
    assert!(recent.json.exists() && workdir_live.json.exists() && root_live.json.exists());
    assert_eq!(
        registry
            .try_health_counts()
            .unwrap()
            .retired_undelivered_completions,
        1
    );
}

#[test]
fn watchdog_cost_scheduler_does_not_own_idle_registries() {
    let registry = BgTaskRegistry::default();
    let weak = Arc::downgrade(&registry.inner);
    registry.start_watchdog();
    drop(registry);
    let deadline = Instant::now() + Duration::from_secs(5);
    while weak.strong_count() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        weak.strong_count(),
        0,
        "watchdog keeps an idle project registry alive forever"
    );
}

#[test]
fn watchdog_cost_terminal_reply_acknowledges_only_originating_session() {
    let storage = tempfile::tempdir().unwrap();
    let ctx = crate::context::AppContext::new(
        crate::context::default_language_provider_factory(),
        crate::config::Config::default(),
    );
    let registry = ctx.bash_background();
    for harness in [
        Harness::Runner,
        Harness::Opencode,
        Harness::Pi,
        Harness::Mcp {
            client: "fixture".into(),
        },
    ] {
        registry.set_harness(harness);
        for command in ["bash_watch", "bash_status", "bash"] {
            let (metadata, paths) = fixture(storage.path(), "origin", false, 0);
            registry
                .insert_rehydrated_task(metadata, paths.clone(), true)
                .unwrap();
            let data = serde_json::json!({"task_id": paths.task_id, "status": "completed", "exit_code": 0, "output_preview": "done"});
            let mut foreign = crate::protocol::Response::success("foreign", data.clone());
            crate::response_finalize::finalize_response_with_bg_completions(
                &mut foreign,
                &ctx,
                "other",
                command,
                false,
            );
            assert!(
                !read_task_at(&resolve_task_layout(&paths.session_dir, &paths.task_id).unwrap())
                    .unwrap()
                    .completion_delivered
            );
            let mut text = "done".to_string();
            let mut reply = crate::protocol::Response::success("reply", data);
            crate::response_finalize::finalize_tool_response(
                &mut reply, &mut text, &ctx, "origin", command, false,
            );
            assert!(
                read_task_at(&resolve_task_layout(&paths.session_dir, &paths.task_id).unwrap())
                    .unwrap()
                    .completion_delivered,
                "{command} returned a terminal result but left it undelivered"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn watchdog_cost_adopted_live_pid_uses_kernel_exit_and_delivers_once() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let (mut metadata, paths) = fixture(storage.path(), "session", false, 0);
    metadata.mark_running(child.id(), child.id() as i32);
    metadata.finished_at = None;
    metadata.duration_ms = None;
    write_task_at(
        &resolve_task_layout(&paths.session_dir, &paths.task_id).unwrap(),
        &metadata,
    )
    .unwrap();
    registry
        .insert_rehydrated_task(metadata, paths.clone(), true)
        .unwrap();
    let task = registry.task(&paths.task_id).unwrap();
    work_counts::reset();
    for _ in 0..120 {
        registry.poll_task(&task).unwrap();
        registry.reap_child(&task);
    }
    let counts = work_counts::get();
    fs::write(&paths.exit, b"0").unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    registry.poll_task(&task).unwrap();
    registry.poll_task(&task).unwrap();
    assert_eq!(counts.opens, 0, "adopted live PID polled its marker");
    assert_eq!(
        registry
            .observed_status(&paths.task_id, "session", 0)
            .unwrap()
            .info
            .status,
        BgTaskStatus::Completed
    );
    assert_eq!(
        registry.drain_completions().len(),
        1,
        "exit was delivered more than once"
    );
}

#[test]
fn watchdog_cost_failed_cleanup_deletion_is_retried() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    let (metadata, paths) = fixture(storage.path(), "session", true, 2);
    registry
        .insert_rehydrated_task(metadata, paths.clone(), true)
        .unwrap();
    let hidden = paths.control_dir.join("hidden-metadata.json");
    fs::rename(&paths.json, &hidden).unwrap();
    registry.cleanup_finished(Duration::ZERO);
    assert!(
        registry.task(&paths.task_id).is_some(),
        "failed deletion discarded the only retry candidate"
    );
    fs::rename(hidden, &paths.json).unwrap();
    registry.cleanup_finished(Duration::ZERO);
    assert!(registry.task(&paths.task_id).is_none());
    assert!(!paths.dir.exists());
}

/// Opt-in plain-read census of the live task store; no DB or GC is involved.
#[cfg(unix)]
#[test]
#[ignore = "requires an explicitly supplied live task storage root"]
fn watchdog_cost_live_storage_marker_census() {
    let root = std::env::var_os("AFT_WATCHDOG_CENSUS_ROOT")
        .expect("supply the task store root explicitly");
    let mut running = Vec::new();
    for namespace in fs::read_dir(root).unwrap().flatten() {
        let Ok(sessions) = fs::read_dir(namespace.path().join("bash-tasks")) else {
            continue;
        };
        for session in sessions.flatten() {
            let Ok(folders) = fs::read_dir(session.path()) else {
                continue;
            };
            for folder in folders.flatten() {
                let path = folder.path().join("control/metadata.json");
                let Ok(metadata) = super::super::persistence::read_task(&path) else {
                    continue;
                };
                if metadata.status == BgTaskStatus::Running
                    && metadata
                        .child_pid
                        .is_some_and(|pid| is_recorded_process_alive(pid, metadata.started_at))
                {
                    if let Ok(resolved) = resolve_task_layout(&session.path(), &metadata.task_id) {
                        running.push((metadata.project_root, resolved.paths));
                    }
                }
            }
        }
    }
    let mut per_root = BTreeMap::new();
    work_counts::reset();
    for (root, paths) in &running {
        let _ = read_exit_marker(paths);
        *per_root.entry(root.clone()).or_insert(0usize) += 1;
    }
    let counts = work_counts::get();
    eprintln!("LIVE marker path: {} recorded-live tasks, {} roots, {counts:?}; per-root task counts {:?}; per-minute opens={} reads={} stats={}", running.len(), per_root.len(), per_root.values().collect::<Vec<_>>(), counts.opens*120, (counts.metadata_reads+counts.marker_reads)*120, counts.stats*120);
    assert!(!running.is_empty());
}

#[test]
fn watchdog_cost_persisted_gc_recurs_without_a_new_replay() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    registry.request_persisted_gc(storage.path());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let done = persisted_gc_slots()
            .lock()
            .unwrap()
            .get(&canonicalized_path(storage.path()))
            .is_some_and(|slot| !slot.running);
        if done {
            break;
        }
        assert!(Instant::now() < deadline, "initial GC did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    let (_, paths) = fixture(storage.path(), "later-session", true, 2);
    persisted_gc_slots()
        .lock()
        .unwrap()
        .get_mut(&canonicalized_path(storage.path()))
        .unwrap()
        .last_finished = Some((
        Instant::now() - Duration::from_secs(61),
        "aft-bash-task-gc".into(),
    ));
    registry.request_recurring_persisted_gc();
    while paths.json.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !paths.json.exists(),
        "once-at-replay GC left later delivered bundles forever"
    );
}

#[test]
fn watchdog_cost_gc_recovers_unobserved_exit_in_an_unloaded_worktree() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    let (mut metadata, paths) = fixture(storage.path(), "session", false, 8);
    metadata.mark_running(999_999_999, 999_999_999);
    metadata.started_at = unix_millis() - 9 * 24 * 3600 * 1000;
    metadata.finished_at = None;
    write_task_at(
        &resolve_task_layout(&paths.session_dir, &paths.task_id).unwrap(),
        &metadata,
    )
    .unwrap();
    fs::write(&paths.exit, b"0").unwrap();
    let old = filetime::FileTime::from_unix_time((unix_millis() / 1000) as i64 - 8 * 24 * 3600, 0);
    filetime::set_file_mtime(&paths.exit, old).unwrap();
    filetime::set_file_mtime(&paths.json, old).unwrap();
    registry.maybe_gc_persisted(storage.path()).unwrap();
    assert!(
        !paths.json.exists(),
        "a dead task with an exit marker stayed running forever"
    );
}

#[test]
fn watchdog_cost_inactive_harness_store_is_swept_without_binding_its_worktree() {
    let storage = tempfile::tempdir().unwrap();
    let runner = storage.path().join("runner");
    let opencode = storage.path().join("opencode");
    fs::create_dir_all(&runner).unwrap();
    let (_, paths) = fixture(&opencode, "old-session", true, 2);
    let registry = BgTaskRegistry::default();
    registry.request_persisted_gc(&runner);
    registry.request_recurring_persisted_gc();
    let deadline = Instant::now() + Duration::from_secs(30);
    while paths.json.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !paths.json.exists(),
        "inactive harness needed a deleted worktree to bind again before it could reap history"
    );
}

#[test]
fn watchdog_cost_terminal_reply_ack_fences_a_late_completion_snapshot() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    let (snapshot, paths) = fixture(storage.path(), "session", false, 0);
    registry
        .insert_rehydrated_task(snapshot.clone(), paths.clone(), true)
        .unwrap();
    registry.ack_terminal_result_for_session(&paths.task_id, "session");
    // Rendering may finish after the caller has already received terminal
    // status. Its pre-ack snapshot must not restore a notification afterward.
    registry.enqueue_completion_from_parts(&snapshot, None, Some(&paths), false, None);
    assert!(
        registry.drain_completions().is_empty(),
        "a late completion snapshot undid the reply acknowledgment"
    );
}

#[cfg(unix)]
#[test]
fn watchdog_cost_adopted_exit_with_refused_marker_still_settles() {
    let storage = tempfile::tempdir().unwrap();
    let registry = BgTaskRegistry::default();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let (mut metadata, paths) = fixture(storage.path(), "session", false, 0);
    metadata.mark_running(child.id(), child.id() as i32);
    metadata.finished_at = None;
    metadata.duration_ms = None;
    write_task_at(
        &resolve_task_layout(&paths.session_dir, &paths.task_id).unwrap(),
        &metadata,
    )
    .unwrap();
    registry
        .insert_rehydrated_task(metadata, paths.clone(), true)
        .unwrap();
    let task = registry.task(&paths.task_id).unwrap();
    registry.poll_task(&task).unwrap();
    fs::remove_file(&paths.exit).unwrap();
    fs::create_dir(&paths.exit).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        registry.poll_task(&task).is_err(),
        "a directory marker must be refused"
    );
    assert!(
        !task.is_running(),
        "a dead adopted process stayed running after its marker was refused"
    );
    assert_eq!(registry.drain_completions().len(), 1);
}
