//! Unit tests for parent folder discovery and the session registry.

use super::*;

fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    // Discovery only looks for the `.git` marker, so an empty one suffices.
    std::fs::create_dir_all(dir.join(".git")).unwrap();
}

#[test]
fn discovery_finds_children_and_reports_the_rest_as_skipped_and_outside() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    for name in ["b", "a", "c"] {
        git_init(&root.join(name));
    }
    git_init(&root.join("group").join("nested"));
    std::fs::write(root.join("notes.md"), "notes").unwrap();
    std::fs::create_dir_all(root.join("node_modules").join("pkg")).unwrap();
    std::fs::create_dir_all(root.join(".hidden")).unwrap();
    git_init(&root.join(".hidden").join("repo"));

    let found = discover(&root, 3).unwrap();
    assert_eq!(
        found.children,
        vec![root.join("a"), root.join("b"), root.join("c")]
    );
    assert_eq!(found.skipped, vec![root.join("group").join("nested")]);
    assert_eq!(found.outside, vec![root.join("notes.md")]);
}

#[test]
fn discovery_refuses_checkouts_and_folders_without_repositories() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    assert!(discover(&root, DEFAULT_MAX_CHILD_REPOS).is_none());
    git_init(&root.join("child"));
    assert!(discover(&root, DEFAULT_MAX_CHILD_REPOS).is_some());
    // A subfolder of a checkout keeps the checkout's own indexes.
    assert!(discover(&root.join("child"), DEFAULT_MAX_CHILD_REPOS).is_none());
    std::fs::create_dir_all(root.join("child").join("src")).unwrap();
    assert!(discover(&root.join("child").join("src"), DEFAULT_MAX_CHILD_REPOS).is_none());
}

#[test]
fn default_cap_covers_the_operators_projects_folder() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    for index in 0..39 {
        git_init(&root.join(format!("repo-{index:02}")));
    }
    let found = discover(&root, DEFAULT_MAX_CHILD_REPOS).unwrap();
    assert_eq!(found.children.len(), 39);
    assert!(found.skipped.is_empty());
    let capped = discover(&root, 32).unwrap();
    assert_eq!(capped.children.len(), 32);
    assert_eq!(capped.skipped.len(), 7);
}

#[test]
fn activate_requires_prepare_and_deactivate_stops_the_session() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    git_init(&root.join("child"));
    let storage = tempfile::tempdir().unwrap();
    let semantic = crate::config::SemanticBackendConfig::default();
    assert!(!activate(&root, storage.path(), &semantic));
    assert!(session_for_root(&root).is_none());
    let planes = RequestedPlanes {
        trigram: true,
        semantic: false,
        callgraph: true,
    };
    assert!(prepare(&root, planes));
    assert!(activate(&root, storage.path(), &semantic));
    let session = session_for_root(&root).unwrap();
    assert!(session.wait_rounds(1, Duration::from_secs(30)));
    // The child has published nothing: each plane is a named gap, and the
    // worker created nothing in the shared storage.
    let child = &session.children()[0];
    assert!(matches!(&*read(&child.trigram), Plane::Gap(_)));
    assert!(matches!(&*read(&child.callgraph), Plane::Gap(_)));
    assert!(matches!(&*read(&child.semantic), Plane::Gap(_)));
    assert_eq!(std::fs::read_dir(storage.path()).unwrap().count(), 0);
    // Rebinding with the same discovery keeps the warm session.
    assert!(prepare(&root, planes));
    assert!(activate(&root, storage.path(), &semantic));
    assert!(Arc::ptr_eq(&session, &session_for_root(&root).unwrap()));
    deactivate(&root);
    assert!(session_for_root(&root).is_none());
    assert!(session.stopped());
}

#[test]
fn inspect_merge_sums_counts_and_prefixes_item_paths() {
    let mut merged = serde_json::Map::new();
    let first = serde_json::json!({"count": 2, "items": [{"file": "src/a.rs"}], "partial": false});
    let second = serde_json::json!({"count": 3, "items": [{"file": "lib/b.rs"}], "partial": true});
    super::inspect::merge(&mut merged, first.as_object().unwrap(), Path::new("alpha"));
    super::inspect::merge(&mut merged, second.as_object().unwrap(), Path::new("beta"));
    assert_eq!(
        serde_json::Value::Object(merged),
        serde_json::json!({
            "count": 5,
            "items": [{"file": "alpha/src/a.rs"}, {"file": "beta/lib/b.rs"}],
            "partial": true,
        })
    );
}
