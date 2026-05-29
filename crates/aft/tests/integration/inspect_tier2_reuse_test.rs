use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use aft::config::Config;
use aft::inspect::{InspectCategory, InspectManager, InspectScanSuccess, InspectSnapshot};
use aft::parser::SymbolCache;

fn write_file(root: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture file has parent")).expect("create parent");
    fs::write(&path, contents).expect("write fixture");
    path
}

fn snapshot(project_root: &Path, inspect_dir: &Path) -> InspectSnapshot {
    let config = Config {
        project_root: Some(project_root.to_path_buf()),
        ..Config::default()
    };
    InspectSnapshot::new(
        project_root.to_path_buf(),
        inspect_dir.to_path_buf(),
        Arc::new(config),
        Arc::new(RwLock::new(SymbolCache::new())),
    )
}

fn duplicate_source() -> String {
    r#"
export function calculate(input: number) {
  const first = input + 1;
  const second = first + 2;
  const third = second + first;
  const fourth = third + 3;
  const fifth = fourth + third;
  const sixth = fifth + second;
  return sixth + fourth;
}
"#
    .to_string()
}

fn changed_source() -> String {
    r#"
export function calculate(input: number) {
  const first = input + 10;
  const second = first + 20;
  const third = second + first;
  const fourth = third + 30;
  const fifth = fourth + third;
  const sixth = fifth + second;
  const seventh = sixth + fifth;
  return seventh + fourth;
}
"#
    .to_string()
}

fn build_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path().join("project");
    fs::create_dir_all(&root).expect("create project");
    let source = duplicate_source();
    let mut mutated_file = PathBuf::new();
    for index in 0..32 {
        let relative = format!("src/file_{index:02}.ts");
        let path = write_file(&root, &relative, &source);
        if index == 7 {
            mutated_file = path;
        }
    }
    (temp_dir, root, mutated_file)
}

fn run_reuse(
    manager: &InspectManager,
    snapshot: InspectSnapshot,
) -> (InspectScanSuccess, Duration) {
    let started = Instant::now();
    let result = manager.tier2_run_with_reuse_result(snapshot, InspectCategory::Duplicates, None);
    let elapsed = started.elapsed();
    (result.outcome.expect("tier2 reuse run succeeds"), elapsed)
}

fn relative_paths(project_root: &Path, files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .map(|file| {
            file.strip_prefix(project_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn inspect_tier2_reuse_skips_fresh_files_and_rescans_stale_file() {
    let (_temp_dir, root, mutated_file) = build_fixture();
    let inspect_dir = root.join(".aft-cache").join("inspect");

    let first_manager = InspectManager::new();
    let (first, t1) = run_reuse(&first_manager, snapshot(&root, &inspect_dir));
    assert_eq!(first.scanned_files.len(), 32);
    assert!(first.aggregate["groups_count"].as_u64().unwrap_or(0) > 0);

    let second_manager = InspectManager::new();
    let (second, t2) = run_reuse(&second_manager, snapshot(&root, &inspect_dir));
    assert!(
        t2 < t1 / 2 || t1.saturating_sub(t2) > Duration::from_millis(5),
        "cached Tier 2 run should be substantially faster: first={t1:?} second={t2:?}"
    );
    assert!(second.scanned_files.is_empty());
    assert_eq!(second.aggregate, first.aggregate);

    fs::write(&mutated_file, changed_source()).expect("mutate one fixture file");

    let third_manager = InspectManager::new();
    let (third, _t3) = run_reuse(&third_manager, snapshot(&root, &inspect_dir));
    assert_eq!(
        relative_paths(&root, &third.scanned_files),
        vec!["src/file_07.ts"]
    );
    assert_ne!(third.aggregate, first.aggregate);
}
