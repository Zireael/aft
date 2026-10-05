//! Checkout semantic views through the runtime that serves views-on search:
//! shared planes across checkouts, embed counts across sessions, and edits
//! reaching the fill through both the watcher and AFT's own writes.

use super::*;
use crate::semantic_index::{EmbedTextCaps, SemanticIndex, SemanticResult};
use std::sync::atomic::AtomicUsize;

const FILES: &[(&str, &str)] = &[
    (
        "src/alpha.rs",
        "pub fn alpha_total(values: &[u32]) -> u32 {\n    values.iter().sum()\n}\n\npub fn alpha_label() -> &'static str {\n    \"alpha\"\n}\n",
    ),
    (
        "src/beta.rs",
        "pub struct BetaCache {\n    entries: Vec<String>,\n}\n\nimpl BetaCache {\n    pub fn insert(&mut self, value: String) {\n        self.entries.push(value);\n    }\n}\n",
    ),
    (
        "src/gamma.rs",
        "fn gamma_parse(input: &str) -> Option<u64> {\n    input.trim().parse().ok()\n}\n",
    ),
    ("notes.txt", "not a semantic source\n"),
];

/// A deterministic model: equal texts get equal vectors.
fn vector(text: &str) -> Vec<f32> {
    blake3::hash(text.as_bytes()).as_bytes()[..16]
        .iter()
        .map(|byte| (f32::from(*byte) - 127.5) / 127.5)
        .collect()
}

#[derive(Default)]
struct Model {
    texts: AtomicUsize,
    calls: AtomicUsize,
}

impl Model {
    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        self.texts.fetch_add(texts.len(), Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(texts.iter().map(|text| vector(text)).collect())
    }

    fn texts(&self) -> usize {
        self.texts.load(Ordering::SeqCst)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn write_tree(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

fn producer() -> SemanticProducer {
    SemanticProducer::current("runtime-test-model", EmbedTextCaps::default())
}

fn open(storage: &Path, scope: &str, root: &Path) -> CheckoutSemantic {
    let runtime =
        CheckoutSemantic::new(storage, "family", scope, root, producer(), Weak::new()).unwrap();
    runtime.driver().register_write_intent();
    runtime.load().unwrap();
    runtime
}

fn refresh(runtime: &CheckoutSemantic, model: &Model) -> FillReport {
    runtime
        .refresh(FillBudget::default(), &mut |texts| model.embed(texts))
        .unwrap()
}

type Row = (String, String, u32, u32);

fn rows(root: &Path, results: &[SemanticResult]) -> Vec<Row> {
    results
        .iter()
        .map(|result| {
            (
                result
                    .file
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                result.name.clone(),
                result.start_line,
                result.score.to_bits(),
            )
        })
        .collect()
}

/// A full legacy build of `root`: its ranked rows for `query` and the number
/// of texts it embedded, which is the number of unique chunks.
fn cold(root: &Path, query: &str) -> (Vec<Row>, usize) {
    let files = ConfiguredMembershipWalker.files(root).unwrap();
    let mut texts = 0;
    let index = SemanticIndex::build(
        root,
        &files,
        &mut |batch: Vec<String>| {
            texts += batch.len();
            Ok(batch.iter().map(|text| vector(text)).collect())
        },
        64,
    )
    .unwrap();
    (rows(root, &index.search(&vector(query), 100)), texts)
}

fn query(runtime: &CheckoutSemantic, text: &str) -> SemanticQuery {
    runtime.search(&vector(text), 100, &|_| true).unwrap()
}

use super::super::first_load::MembershipWalker;

/// Two worktrees of one family with identical content embed each chunk once
/// between them; a later session on the same worktrees embeds nothing and
/// ranks exactly like a legacy build of the same content.
#[test]
fn two_worktrees_two_sessions_embed_each_chunk_once() {
    let storage = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_tree(first.path(), FILES);
    write_tree(second.path(), FILES);
    let (expected, chunks) = cold(first.path(), "beta cache insert");
    assert!(chunks > 0);

    let model = Model::default();
    {
        let a = open(storage.path(), "worktree-a", first.path());
        let b = open(storage.path(), "worktree-b", second.path());
        assert!(Arc::ptr_eq(a.plane(), b.plane()), "one plane per storage");
        refresh(&a, &model);
        refresh(&b, &model);
        assert_eq!(model.texts(), chunks, "identical content embedded twice");
        let answer = query(&b, "beta cache insert");
        assert!(answer.complete(), "second worktree incomplete: {answer:?}");
        assert_eq!(rows(second.path(), &answer.results), expected);
    }

    // Session 2: the first session's checkouts and plane are gone.
    let embedded = model.texts();
    let a = open(storage.path(), "worktree-a", first.path());
    let b = open(storage.path(), "worktree-b", second.path());
    refresh(&a, &model);
    refresh(&b, &model);
    assert_eq!(model.texts(), embedded, "a later session embedded again");
    for (runtime, root) in [(&a, first.path()), (&b, second.path())] {
        let answer = query(runtime, "beta cache insert");
        assert!(answer.complete(), "incomplete after restart: {answer:?}");
        assert_eq!(rows(root, &answer.results), expected);
    }
}

#[test]
fn family_fill_rechecks_completed_work_after_acquiring_claim() {
    let storage = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let files = [("src/file.rs", "pub fn one_chunk() -> u8 { 1 }\n")];
    write_tree(first.path(), &files);
    write_tree(second.path(), &files);
    let a = open(storage.path(), "a", first.path());
    let b = Arc::new(open(storage.path(), "b", second.path()));
    let model = Arc::new(Model::default());
    let callback_model = Arc::clone(&model);
    let callback_b = Arc::clone(&b);
    // Finish the competing fill exactly between a's store miss and claim.
    // No sleep or host scheduling assumption is involved.
    super::super::semantic::before_claim_for_test(move || {
        let report = refresh(&callback_b, &callback_model);
        assert_eq!((report.embedded_keys, report.installed), (1, 1));
    });
    let report = refresh(&a, &model);
    let usage = a
        .owner
        .open_store(crate::blob_store::v2::FamilyPlane::Semantic)
        .unwrap()
        .usage()
        .unwrap();
    eprintln!(
        "completed claim race: texts={} calls={} outer_embedded={} stored_rows={}",
        model.texts(),
        model.calls(),
        report.embedded_keys,
        usage.rows
    );
    assert!(query(&a, "one chunk").complete());
    assert_eq!(
        rows(first.path(), &query(&a, "one chunk").results),
        cold(first.path(), "one chunk").0
    );
    assert_eq!(
        model.texts(),
        1,
        "a completed claim must not embed the same stored key again"
    );
    assert_eq!(model.calls(), 1);
    assert_eq!(report.embedded_keys, 0);
    assert_eq!(usage.rows, 1);
    assert_eq!(
        a.installed()
            .generation()
            .manifest()
            .to_json_bytes()
            .unwrap(),
        b.installed()
            .generation()
            .manifest()
            .to_json_bytes()
            .unwrap()
    );
    assert_eq!(
        rows(first.path(), &query(&a, "one chunk").results),
        rows(second.path(), &query(&b, "one chunk").results)
    );
}

#[test]
fn completed_embedding_batches_survive_a_later_batch_failure() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_tree(
        root.path(),
        &[
            ("a.rs", "pub fn first_chunk() -> u8 { 1 }\n"),
            ("b.rs", "pub fn second_chunk() -> u8 { 2 }\n"),
            ("c.rs", "pub fn third_chunk() -> u8 { 3 }\n"),
        ],
    );
    let runtime = open(storage.path(), "batch-failure", root.path());
    let model = Model::default();
    let budget = FillBudget {
        max_batch: 1,
        ..FillBudget::default()
    };
    let report = runtime
        .refresh(budget, &mut |texts| {
            let vectors = model.embed(texts)?;
            if model.calls() == 2 {
                Err("embedding backend interrupted".into())
            } else {
                Ok(vectors)
            }
        })
        .unwrap();
    let store = runtime
        .owner
        .open_store(crate::blob_store::v2::FamilyPlane::Semantic)
        .unwrap();
    eprintln!(
        "later batch failure: texts={} calls={} embedded_keys={} installed={} rows={}",
        model.texts(),
        model.calls(),
        report.embedded_keys,
        report.installed,
        store.usage().unwrap().rows
    );
    assert_eq!(report.errors.len(), 1);
    assert_eq!(
        (report.embedded_keys, report.installed),
        (1, 1),
        "a later batch error must not discard an already complete file run"
    );
    assert_eq!(store.usage().unwrap().rows, 1);
    runtime
        .refresh(budget, &mut |texts| model.embed(texts))
        .unwrap();
    assert_eq!(
        model.texts(),
        4,
        "retry must not re-embed the completed first file"
    );
    assert_eq!(model.calls(), 4);
    assert_eq!(store.usage().unwrap().rows, 3);
    let answer = query(&runtime, "chunk");
    assert!(answer.complete());
    assert_eq!(
        rows(root.path(), &answer.results),
        cold(root.path(), "chunk").0
    );
}

#[test]
fn repeated_worktree_edits_with_periodic_backend_failures_never_repeat_stored_texts() {
    let storage = tempfile::tempdir().unwrap();
    let roots: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    for root in &roots {
        for i in 0..2048 {
            std::fs::write(
                root.path().join(format!("file_{i:04}.rs")),
                format!("pub fn item_{i:04}() -> u32 {{ {i} }}\n"),
            )
            .unwrap();
        }
    }
    let runtimes: Vec<_> = roots
        .iter()
        .enumerate()
        .map(|(i, root)| open(storage.path(), &format!("flaky-worktree-{i}"), root.path()))
        .collect();
    let store = runtimes[0]
        .owner
        .open_store(crate::blob_store::v2::FamilyPlane::Semantic)
        .unwrap();
    let model = Model::default();
    let completed = std::cell::RefCell::new(std::collections::HashSet::new());
    let successful_texts = std::cell::Cell::new(0usize);
    let mut failed_texts = 0usize;
    let mut embed = |texts: Vec<String>| {
        // One chunk per file makes completed text count independent of the
        // actual store's row count. Each completed request must already have
        // reached the store before another request is sent.
        assert_eq!(
            store.usage().unwrap().rows as usize,
            successful_texts.get(),
            "completed batches were not persisted before the next request"
        );
        for text in &texts {
            assert!(
                !completed.borrow().contains(text),
                "a stored text was embedded again after retry"
            );
        }
        let count = texts.len();
        let retained = texts.clone();
        let vectors = model.embed(texts)?;
        if model.calls().is_multiple_of(7) {
            failed_texts += count;
            Err("embedding backend interrupted every seventh batch".into())
        } else {
            completed.borrow_mut().extend(retained);
            successful_texts.set(successful_texts.get() + count);
            Ok(vectors)
        }
    };
    let budget = FillBudget {
        max_files: 4096,
        ..FillBudget::default()
    };
    let mut new_keys = 0;
    for runtime in &runtimes {
        let before = model.calls();
        for attempt in 0..64 {
            let report = runtime.refresh(budget, &mut embed).unwrap();
            new_keys += report.embedded_keys;
            if report.errors.is_empty() && report.deferred == 0 {
                break;
            }
            assert!(attempt < 63, "fixture failed to settle");
        }
        assert!(query(runtime, "item").complete());
        if runtime.access.scope() != "flaky-worktree-0" {
            assert_eq!(
                model.calls(),
                before,
                "a sibling must reuse all stored vectors"
            );
        }
    }
    for round in 0..8 {
        // A new whole-file key can legitimately have the same chunk text
        // (the template need not include a changed body). Reject duplicates
        // within a content revision/retry schedule, not across new keys.
        completed.borrow_mut().clear();
        let source = format!("pub fn revised_item() -> u32 {{ {} }}\n", round + 9000);
        for (i, (runtime, root)) in runtimes.iter().zip(&roots).enumerate() {
            let file = root.path().join("file_0000.rs");
            std::fs::write(&file, &source).unwrap();
            runtime.driver.record_absolute_change(&file);
            let before = model.calls();
            for attempt in 0..64 {
                let report = runtime.refresh(budget, &mut embed).unwrap();
                new_keys += report.embedded_keys;
                if report.errors.is_empty() && report.deferred == 0 {
                    break;
                }
                assert!(attempt < 63, "fixture failed to settle");
            }
            if i > 0 {
                assert_eq!(
                    model.calls(),
                    before,
                    "same-content sibling edit sent a model request"
                );
            }
            assert!(query(runtime, "revised item").complete());
        }
    }
    drop(embed);
    assert_eq!(successful_texts.get(), 2056);
    assert_eq!(new_keys, 2056);
    assert_eq!(store.usage().unwrap().rows, 2056);
    assert_eq!(failed_texts, 321);
    assert_eq!(model.texts(), 2377);
    assert_eq!(model.calls(), 46);
    for (runtime, root) in runtimes.iter().zip(&roots) {
        assert_eq!(
            rows(root.path(), &query(runtime, "revised item").results),
            cold(root.path(), "revised item").0
        );
    }
    eprintln!("periodic failure, three worktrees: successful texts={} failed texts={failed_texts} sent texts={} calls={} new keys={new_keys} stored rows={}", successful_texts.get(), model.texts(), model.calls(), store.usage().unwrap().rows);
}

#[test]
fn repeated_worktree_edits_embed_only_new_family_keys_even_after_dropped_admission() {
    let storage = tempfile::tempdir().unwrap();
    let roots: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    for root in &roots {
        for i in 0..2048 {
            std::fs::write(
                root.path().join(format!("file_{i:04}.rs")),
                format!("pub fn item_{i:04}() -> u32 {{ {i} }}\n"),
            )
            .unwrap();
        }
    }
    let runtimes: Vec<_> = roots
        .iter()
        .enumerate()
        .map(|(i, root)| open(storage.path(), &format!("worktree-{i}"), root.path()))
        .collect();
    let model = Model::default();
    let budget = FillBudget {
        max_files: 4096,
        ..FillBudget::default()
    };
    let mut embedded_keys = 0;
    for (i, runtime) in runtimes.iter().enumerate() {
        let report = runtime
            .refresh(budget, &mut |texts| model.embed(texts))
            .unwrap();
        embedded_keys += report.embedded_keys;
        assert_eq!(report.embedded_keys, if i == 0 { 2048 } else { 0 });
        assert_eq!(report.model_calls, if i == 0 { 32 } else { 0 });
    }
    let store = runtimes[0]
        .owner
        .open_store(crate::blob_store::v2::FamilyPlane::Semantic)
        .unwrap();
    for runtime in &runtimes[1..] {
        assert_eq!(
            runtime
                .owner
                .open_store(crate::blob_store::v2::FamilyPlane::Semantic)
                .unwrap()
                .path(),
            store.path()
        );
    }
    for round in 0..8 {
        let source = format!("pub fn revised_item() -> u32 {{ {} }}\n", round + 9000);
        for (i, (runtime, root)) in runtimes.iter().zip(&roots).enumerate() {
            let file = root.path().join("file_0000.rs");
            {
                let _intent = crate::views::intent::record_paths([file.as_path()]);
                std::fs::write(&file, &source).unwrap();
            }
            let before = model.texts();
            let report = runtime
                .refresh(budget, &mut |texts| model.embed(texts))
                .unwrap();
            embedded_keys += report.embedded_keys;
            assert_eq!(report.embedded_keys, usize::from(i == 0));
            assert_eq!(report.model_calls, usize::from(i == 0));
            assert_eq!(
                model.texts() - before,
                usize::from(i == 0),
                "second/third worktree repeated an embedding"
            );
            // Repeated no-op writes to the same content must not send texts.
            let _intent = crate::views::intent::record_paths([file.as_path()]);
            std::fs::write(&file, &source).unwrap();
            drop(_intent);
            let report = runtime
                .refresh(budget, &mut |texts| model.embed(texts))
                .unwrap();
            assert_eq!((report.embedded_keys, report.model_calls), (0, 0));
        }
    }
    assert_eq!(model.texts(), 2056);
    assert_eq!(model.calls(), 40);
    assert_eq!(embedded_keys, 2056);
    assert_eq!(store.usage().unwrap().rows, 2056);

    // A source completion rejected at admission still stores its immutable
    // payload first. Retry and a sibling must reuse it, not re-embed it.
    let a = &runtimes[0];
    let source = "pub fn admission_was_cancelled() -> u32 { 99999 }\n";
    let file = roots[0].path().join("file_0000.rs");
    std::fs::write(&file, source).unwrap();
    a.driver.record_absolute_change(&file);
    let snapshot = a
        .loader
        .refresh(&a.access, Arc::clone(a.installed().generation()))
        .unwrap();
    let path = super::super::RelPath::new(b"file_0000.rs".to_vec()).unwrap();
    let mut moved = super::super::snapshot::LiveDelta::new(Arc::clone(snapshot.generation()));
    moved.apply(
        path,
        super::super::snapshot::LiveEntry::new(
            super::super::snapshot::DiskState::of_bytes(b"other bytes"),
            1,
        ),
    );
    let cancelled = moved.snapshot();
    let report = a
        .plane
        .fill(
            &a.owner,
            &snapshot,
            budget,
            &mut |texts| model.embed(texts),
            &|| cancelled.clone(),
        )
        .unwrap();
    assert_eq!(
        (report.embedded_keys, report.installed, report.dropped),
        (1, 0, 1)
    );
    let before = model.texts();
    let retry = a.refresh(budget, &mut |texts| model.embed(texts)).unwrap();
    assert_eq!(
        (retry.embedded_keys, retry.model_calls, retry.installed),
        (0, 0, 1)
    );
    let file = roots[1].path().join("file_0000.rs");
    std::fs::write(&file, source).unwrap();
    runtimes[1].driver.record_absolute_change(&file);
    let sibling = runtimes[1]
        .refresh(budget, &mut |texts| model.embed(texts))
        .unwrap();
    assert_eq!((sibling.embedded_keys, sibling.model_calls), (0, 0));
    assert_eq!(model.texts(), before);
    assert_eq!(model.texts(), 2057);
    assert_eq!(model.calls(), 41);
    assert_eq!(store.usage().unwrap().rows, 2057);
    for (runtime, root) in runtimes.iter().zip(&roots) {
        let answer = query(runtime, "revised item");
        assert!(answer.complete());
        assert_eq!(
            rows(root.path(), &answer.results),
            cold(root.path(), "revised item").0
        );
    }
    eprintln!("three worktrees, repeated edits and cancelled admission: texts={} calls={} stored_rows={} payload_bytes={}", model.texts(), model.calls(), store.usage().unwrap().rows, store.usage().unwrap().payload_bytes);
}

/// An edit the watcher reports and an edit AFT writes each reach the fill:
/// the new content is embedded and the next query scores it.
#[test]
fn watcher_and_aft_edits_reach_the_fill() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    write_tree(&root_path, FILES);
    let runtime = open(storage.path(), "scope", &root_path);
    let model = Model::default();
    refresh(&runtime, &model);

    // Outside edit: the watcher drain hands the path to the driver.
    let before = model.texts();
    write_tree(
        &root_path,
        &[(
            "src/gamma.rs",
            "pub fn gamma_watched_rewrite() -> u8 {\n    3\n}\n",
        )],
    );
    runtime
        .driver()
        .record_absolute_change(&root_path.join("src/gamma.rs"));
    let pending = query(&runtime, "gamma watched rewrite");
    assert!(
        pending.pending.contains(&root_path.join("src/gamma.rs")),
        "an unreconciled edit must be a named gap: {pending:?}"
    );
    let report = refresh(&runtime, &model);
    assert!(report.embedded_keys >= 1 && model.texts() > before);
    let answer = query(&runtime, "gamma watched rewrite");
    assert!(answer.complete(), "{answer:?}");
    assert_eq!(
        rows(&root_path, &answer.results),
        cold(&root_path, "gamma watched rewrite").0
    );

    // AFT write: the write-intent listener records it before the bytes land.
    let before = model.texts();
    let target = root_path.join("src/alpha.rs");
    {
        let _intent = crate::views::intent::record_paths([target.as_path()]);
        std::fs::write(&target, "pub fn alpha_written_by_aft() -> u8 {\n    1\n}\n").unwrap();
    }
    assert!(
        query(&runtime, "alpha written").pending.contains(&target),
        "an acknowledged write must be a named gap until reconciled"
    );
    refresh(&runtime, &model);
    assert!(model.texts() > before, "the AFT write was never embedded");
    let answer = query(&runtime, "alpha written by aft");
    assert!(answer.complete(), "{answer:?}");
    assert_eq!(
        rows(&root_path, &answer.results),
        cold(&root_path, "alpha written by aft").0
    );
}

fn pending_semantic(runtime: &CheckoutSemantic) -> usize {
    runtime
        .installed()
        .generation()
        .manifest()
        .entries()
        .filter(|(_, entry)| {
            entry
                .plane_state(crate::blob_store::v2::FamilyPlane::Semantic)
                .is_some_and(|state| state.is_pending())
        })
        .count()
}

fn large_pin_fixture() -> (tempfile::TempDir, tempfile::TempDir, CheckoutSemantic) {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for i in 0..2048 {
        std::fs::write(
            root.path().join(format!("file_{i:04}.rs")),
            format!("pub fn item_{i:04}() -> u32 {{ {i} }}\n"),
        )
        .unwrap();
    }
    let runtime = open(storage.path(), "pins", root.path());
    (storage, root, runtime)
}

fn fill_large(runtime: &CheckoutSemantic) {
    let report = runtime
        .plane()
        .fill(
            &runtime.owner,
            &runtime.installed(),
            FillBudget {
                max_files: 4096,
                ..FillBudget::default()
            },
            &mut |texts| Model::default().embed(texts),
            &|| runtime.installed(),
        )
        .unwrap();
    assert_eq!(
        (
            report.installed,
            report.embedded_keys,
            report.embedded_texts
        ),
        (2048, 2048, 2048)
    );
}

#[test]
fn large_semantic_fill_batches_pin_writes() {
    use crate::pins::work_counters::{key_file_bytes, key_file_work};
    let (_storage, root, runtime) = large_pin_fixture();
    let (writes, syncs) = key_file_work();
    let bytes = key_file_bytes();
    fill_large(&runtime);
    let after = key_file_work();
    eprintln!(
        "fill pins: writes={} bytes={} key syncs={}",
        after.0 - writes,
        key_file_bytes() - bytes,
        after.1 - syncs
    );
    let answer = query(&runtime, "item");
    assert!(answer.complete());
    assert_eq!(
        rows(root.path(), &answer.results),
        cold(root.path(), "item").0
    );
    assert_eq!((after.0 - writes, after.1 - syncs), (1, 0));
    assert_eq!(key_file_bytes() - bytes, 2048 * 65);
}

#[test]
fn large_semantic_materialization_batches_pins_and_touches() {
    use crate::pins::work_counters::{key_file_bytes, key_file_work};
    let (_storage, root, runtime) = large_pin_fixture();
    fill_large(&runtime);
    let answer = query(&runtime, "item");
    let (writes, syncs) = key_file_work();
    let bytes = key_file_bytes();
    crate::blob_store::v2::take_touch_transactions();
    let (snapshot, revision) = runtime.driver.installed_cut();
    runtime
        .loader
        .fold(&runtime.access, &snapshot, revision)
        .unwrap();
    let after = key_file_work();
    let transactions = crate::blob_store::v2::take_touch_transactions();
    let written_bytes = key_file_bytes() - bytes;
    eprintln!(
        "fold pins: writes={} bytes={} key syncs={} touch transactions={transactions}",
        after.0 - writes,
        written_bytes,
        after.1 - syncs
    );
    assert_eq!(pending_semantic(&runtime), 0);
    assert_eq!(
        rows(root.path(), &query(&runtime, "item").results),
        rows(root.path(), &answer.results)
    );
    let manifest = runtime
        .installed()
        .generation()
        .manifest()
        .to_json_bytes()
        .unwrap();
    let store = runtime
        .owner
        .open_store(crate::blob_store::v2::FamilyPlane::Semantic)
        .unwrap();
    let blobs: Vec<_> = runtime
        .installed()
        .generation()
        .manifest()
        .ready_keys()
        .map(|key| (key, store.get(&key).unwrap().unwrap()))
        .collect();
    runtime.load().unwrap();
    assert_eq!(
        runtime
            .installed()
            .generation()
            .manifest()
            .to_json_bytes()
            .unwrap(),
        manifest
    );
    for (key, payload) in blobs {
        assert_eq!(store.get(&key).unwrap().unwrap(), payload);
    }
    assert_eq!((after.0 - writes, after.1 - syncs), (2, 0));
    assert_eq!(written_bytes, 2048 * 65 * 2);
    assert_eq!(transactions, 1);
}

/// A large, already-filled checkout must not read or embed unchanged sources
/// again just to publish one edit. Compare to the strict loader and an
/// independent cold index, not to another copy of the fast path.
#[test]
fn large_checkout_edit_counts_source_work_and_preserves_outputs() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    for i in 0..2048 {
        let path = root.join(format!("file_{i:04}.rs"));
        std::fs::write(&path, format!("pub fn item_{i:04}() -> u32 {{ {i} }}\n")).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(60)),
            )
            .unwrap();
    }
    let runtime = open(storage.path(), "large", &root);
    let model = Model::default();
    let budget = FillBudget {
        max_files: 4096,
        ..FillBudget::default()
    };
    runtime
        .refresh(budget, &mut |texts| model.embed(texts))
        .unwrap();
    runtime.driver().take_reconcile_work();
    let before = model.texts();
    let target = root.join("file_0000.rs");
    let changed = "pub fn rewritten_item() -> u32 { 9999 }\n";
    {
        let _intent = crate::views::intent::record_paths([target.as_path()]);
        std::fs::write(&target, changed).unwrap();
    }
    let report = runtime
        .refresh(budget, &mut |texts| model.embed(texts))
        .unwrap();
    let work = runtime.driver().take_reconcile_work();
    eprintln!(
        "edit: {work:?}; embedded texts={}, keys={}",
        model.texts() - before,
        report.embedded_keys
    );
    assert_eq!(
        model.texts() - before,
        1,
        "unchanged content was re-embedded"
    );
    let answer = query(&runtime, "rewritten item");
    assert!(answer.complete(), "{answer:?}");
    assert_eq!(
        rows(&root, &answer.results),
        cold(&root, "rewritten item").0
    );
    let snapshot = runtime.installed();
    let manifest = snapshot.generation().manifest().to_json_bytes().unwrap();
    let store = runtime
        .owner
        .open_store(crate::blob_store::v2::FamilyPlane::Semantic)
        .unwrap();
    let blobs: Vec<_> = snapshot
        .generation()
        .manifest()
        .ready_keys()
        .map(|key| (key, store.get(&key).unwrap().unwrap()))
        .collect();
    runtime.load().unwrap();
    assert_eq!(
        runtime
            .installed()
            .generation()
            .manifest()
            .to_json_bytes()
            .unwrap(),
        manifest
    );
    for (key, payload) in blobs {
        assert_eq!(store.get(&key).unwrap().unwrap(), payload);
    }
    assert_eq!(
        rows(&root, &query(&runtime, "rewritten item").results),
        rows(&root, &answer.results)
    );
    assert_eq!(
        work.walks, 1,
        "an edit needs one strict membership reconciliation, not fold walks"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            work.reads, 1,
            "unchanged files should cost a stat, not a source read"
        );
        assert_eq!(work.content_hash_bytes, changed.len());
        assert_eq!(work.attachments, 1);
        assert_eq!(
            work.stats, 2049,
            "one stat per member plus a post-read stability stat"
        );
    }
}

/// A catch-up that takes several budgeted rounds serves each round's vectors
/// at once but publishes them in one fold at the end, because every fold
/// re-walks the whole checkout.
#[test]
fn budgeted_rounds_fold_once_when_caught_up() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_tree(root.path(), FILES);
    let runtime = open(storage.path(), "scope", root.path());
    let model = Model::default();
    let one_file = FillBudget {
        max_files: 1,
        ..FillBudget::default()
    };
    assert_eq!(pending_semantic(&runtime), 3);
    let first = runtime
        .refresh(one_file, &mut |texts| model.embed(texts))
        .unwrap();
    assert_eq!((first.installed, first.deferred), (1, 2));
    let generation = runtime.installed().generation().name().to_owned();
    assert_eq!(pending_semantic(&runtime), 3, "folded before catching up");
    assert_eq!(query(&runtime, "alpha").pending.len(), 2);
    runtime
        .refresh(one_file, &mut |texts| model.embed(texts))
        .unwrap();
    assert_eq!(runtime.installed().generation().name(), generation);
    let last = runtime
        .refresh(one_file, &mut |texts| model.embed(texts))
        .unwrap();
    assert_eq!((last.installed, last.deferred), (1, 0));
    assert_eq!(pending_semantic(&runtime), 0, "caught up but never folded");
    assert!(query(&runtime, "alpha").complete());
}

fn fast_schedule(root: &Path) -> FillSchedule {
    FillSchedule {
        root: root.to_path_buf(),
        quiet_window: Duration::from_millis(10),
        retry_initial: Duration::from_millis(20),
        retry_max: Duration::from_millis(80),
        limiter: crate::cold_build_limiter::isolated_limiter(1),
        paused: Box::new(|| false),
        max_batch: 64,
    }
}

/// A lane whose checkout is loaded and served, as the worker leaves it.
fn served_lane(
    storage: &Path,
    root: &Path,
) -> (
    Arc<CheckoutSemanticSlot>,
    u64,
    crossbeam_channel::Receiver<()>,
    Arc<CheckoutSemantic>,
) {
    let slot = Arc::new(CheckoutSemanticSlot::default());
    let (epoch, wake) = slot.begin();
    let runtime = Arc::new(
        CheckoutSemantic::new(
            storage,
            "family",
            "scope",
            root,
            producer(),
            Arc::downgrade(&slot),
        )
        .unwrap(),
    );
    runtime.load().unwrap();
    assert!(slot.install(epoch, Arc::clone(&runtime)));
    (slot, epoch, wake, runtime)
}

/// A fill that fails because the embedding backend is down is retried on a
/// backoff: with no edit, wake or reconfigure, the view becomes complete once
/// the backend answers again.
#[test]
fn transient_embed_errors_retry_until_complete_without_a_wake() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_tree(root.path(), FILES);
    let (slot, epoch, wake, runtime) = served_lane(storage.path(), root.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = {
        let weak = Arc::downgrade(&slot);
        let calls = Arc::clone(&calls);
        let root = root.path().to_path_buf();
        std::thread::spawn(move || {
            let schedule = fast_schedule(&root);
            serve_fills(&weak, epoch, &wake, &schedule, &mut |texts: Vec<String>| {
                // The backend is unreachable for the first three calls.
                if calls.fetch_add(1, Ordering::SeqCst) < 3 {
                    return Err("embedding backend unreachable".to_string());
                }
                Ok(texts.iter().map(|text| vector(text)).collect())
            });
        })
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let complete = loop {
        if query(&runtime, "alpha").complete() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    slot.clear();
    worker.join().unwrap();
    assert!(
        complete,
        "a transient embed error left the view partial with no retry"
    );
    assert!(calls.load(Ordering::SeqCst) >= 4);
}

/// A superseded runtime of the same view (a lane restarted with the same
/// producer) must not release the state of the runtime that replaced it.
#[test]
fn dropping_a_superseded_runtime_keeps_the_replacements_view() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_tree(root.path(), FILES);
    let old = open(storage.path(), "scope", root.path());
    let new = open(storage.path(), "scope", root.path());
    assert!(Arc::ptr_eq(old.plane(), new.plane()));
    refresh(&new, &Model::default());
    drop(old);
    let answer = new
        .search(&vector("alpha"), 10, &|_| true)
        .expect("the replacement lost its resident generation");
    assert!(
        answer.complete(),
        "the replacement lost its fills: {answer:?}"
    );
}

/// Every `CheckoutSemantic::new` holds its view once and every drop releases
/// it once, keyed by the plane's never-reused id rather than its address.
#[test]
fn live_view_holders_match_runtimes_and_planes_have_unique_ids() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_tree(root.path(), FILES);
    let first = open(storage.path(), "holders", root.path());
    let plane = Arc::clone(first.plane());
    let access = first.access().clone();
    assert_eq!(live_holders(&plane, &access), 1);
    let second = open(storage.path(), "holders", root.path());
    assert_eq!(live_holders(&plane, &access), 2);
    drop(first);
    assert_eq!(live_holders(&plane, &access), 1);
    drop(second);
    assert_eq!(live_holders(&plane, &access), 0);

    let id = plane.id();
    drop(plane);
    // A plane built after the previous one was freed, possibly at the same
    // address, still gets an id of its own.
    let replacement = shared_plane(storage.path(), producer());
    assert_ne!(replacement.id(), id);
}

#[test]
fn only_busy_and_checkout_races_are_transient_load_errors() {
    for transient in [
        super::super::first_load::CHECKOUT_CHANGED_DURING_RECONCILE,
        super::super::first_load::CHECKOUT_WRITE_ACTIVE,
        super::super::first_load::CHECKOUT_CHANGED_BEFORE_INSTALL,
        "trigram plane: database is locked",
    ] {
        assert!(is_transient_reason(transient), "{transient}");
    }
    for permanent in [
        "semantic plane: file is not a database",
        "trigram plane: Permission denied (os error 13)",
        "semantic plane: semantic producer mismatch",
        "unsupported persisted format: written by a newer build",
    ] {
        assert!(!is_transient_reason(permanent), "{permanent}");
    }
}

/// A refresh error no timed retry would fix (here the view store cannot be
/// written) becomes the checkout's unavailable reason after one attempt;
/// the worker does not retry it on the backoff. An embedding failure, by contrast, is
/// retried (`transient_embed_errors_retry_until_complete_without_a_wake`).
#[cfg(unix)]
#[test]
fn store_errors_name_the_gap_without_a_retry_loop() {
    use std::os::unix::fs::PermissionsExt;
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_tree(root.path(), FILES);
    let (slot, epoch, wake, runtime) = served_lane(storage.path(), root.path());
    // An edit makes the next refresh reload, which publishes into the view
    // directory; a read-only directory refuses that publication.
    std::fs::write(root.path().join("src/gamma.rs"), "pub fn gamma_edit() {}\n").unwrap();
    runtime
        .driver()
        .record_absolute_change(&root.path().join("src/gamma.rs"));
    let view_dir = crate::views::registry::view_dir(storage.path(), "scope").unwrap();
    let original = std::fs::metadata(&view_dir).unwrap().permissions();
    std::fs::set_permissions(&view_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let worker = {
        let weak = Arc::downgrade(&slot);
        let root = root.path().to_path_buf();
        std::thread::spawn(move || {
            let schedule = fast_schedule(&root);
            serve_fills(&weak, epoch, &wake, &schedule, &mut |texts: Vec<String>| {
                Ok(texts.iter().map(|text| vector(text)).collect())
            });
        })
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while runtime.unavailable_reason().is_none() {
        assert!(Instant::now() < deadline, "the store error was never named");
        std::thread::sleep(Duration::from_millis(5));
    }
    // The backoff (20 ms doubling to 80 ms) would allow several refresh
    // attempts in this wait if the error were treated as transient.
    std::thread::sleep(Duration::from_millis(500));
    let attempts = runtime.refresh_attempts();
    let answer = query(&runtime, "gamma");
    std::fs::set_permissions(&view_dir, original).unwrap();
    slot.clear();
    worker.join().unwrap();
    assert_eq!(attempts, 1, "a store error was retried on the backoff");
    assert!(!answer.complete());
    assert!(
        answer
            .unavailable
            .as_deref()
            .is_some_and(|reason| reason.contains("Permission denied")),
        "{answer:?}"
    );
}

/// A worker superseded by a lane restart (here a model change) is held just
/// before its final publication while the new lane publishes; when released,
/// its publication is refused and the view still names the new producer. No
/// wait for the old worker is involved: the fence alone keeps the old
/// producer's manifest from ever replacing the new one.
#[test]
fn superseded_lane_cannot_publish_over_its_replacement() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_tree(root.path(), FILES);
    let lane = |model: &str, slot: &Arc<CheckoutSemanticSlot>, epoch: u64| {
        let runtime = CheckoutSemantic::new(
            storage.path(),
            "family",
            "scope",
            root.path(),
            SemanticProducer::current(model, EmbedTextCaps::default()),
            Arc::downgrade(slot),
        )
        .unwrap();
        runtime
            .driver()
            .set_publish_fence(Arc::new(LaneFence::new(Arc::downgrade(slot), epoch)));
        Arc::new(runtime)
    };
    let slot = Arc::new(CheckoutSemanticSlot::default());
    let (old_epoch, _old_wake) = slot.begin();
    let old = lane("old-model", &slot, old_epoch);
    old.load().unwrap();

    // The old worker's final fold, held at a barrier before it publishes.
    let (release, barrier) = crossbeam_channel::bounded::<()>(0);
    let final_fold = {
        let old = Arc::clone(&old);
        std::thread::spawn(move || {
            barrier.recv().unwrap();
            old.load()
        })
    };
    let (new_epoch, _new_wake) = slot.begin();
    let new = lane("new-model", &slot, new_epoch);
    new.load().unwrap();
    release.send(()).unwrap();
    let refused = final_fold.join().unwrap();

    let error = refused.expect_err("a superseded lane published its view");
    assert!(
        error.contains(super::super::first_load::SUPERSEDED_PUBLISH),
        "{error}"
    );
    let registry = FamilyRegistry::open(storage.path(), "family").unwrap();
    let view = registry.register_view("scope", root.path()).unwrap();
    let store = view.view_store().unwrap();
    let current = store.current_generation().unwrap().unwrap();
    let manifest = store.load_manifest_v2(&current).unwrap();
    assert_eq!(
        manifest.header().producers.semantic.as_deref(),
        Some(new.plane().semantic_producer().id().as_str()),
        "the view names the superseded producer"
    );
}

/// An AFT write under the root wakes the lane's worker; one elsewhere does not.
#[test]
fn aft_writes_wake_only_their_own_lane() {
    let storage = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    write_tree(&root_path, FILES);
    let slot = Arc::new(CheckoutSemanticSlot::default());
    let (epoch, wake) = slot.begin();
    let runtime = CheckoutSemantic::new(
        storage.path(),
        "family",
        "scope",
        &root_path,
        producer(),
        Arc::downgrade(&slot),
    )
    .unwrap();
    assert!(slot.install(epoch, Arc::new(runtime)));
    {
        let _intent = crate::views::intent::record_paths([other.path().join("x.rs").as_path()]);
    }
    assert!(wake.try_recv().is_err());
    {
        let _intent =
            crate::views::intent::record_paths([root_path.join("src/alpha.rs").as_path()]);
    }
    assert!(wake.try_recv().is_ok());
    slot.clear();
    assert!(slot.runtime().is_none());
    assert!(!slot.install(epoch, Arc::new(open(storage.path(), "scope", &root_path))));
}

/// Resident memory of this plane against the legacy index for the same
/// checkout. Run on a real tree with
/// `AFT_SEMANTIC_MEMORY_ROOT=<checkout> cargo test -p agent-file-tools --lib
/// semantic_runtime::tests::report_resident_memory -- --ignored --nocapture`.
#[test]
#[ignore = "measurement, run by hand on a real checkout"]
fn report_resident_memory() {
    let root = std::env::var_os("AFT_SEMANTIC_MEMORY_ROOT")
        .map(PathBuf::from)
        .expect("AFT_SEMANTIC_MEMORY_ROOT");
    let root = std::fs::canonicalize(root).unwrap();
    // Vectors as wide as the default local model's (384 floats), so the
    // byte counts match what a real embedding run would hold.
    let wide = |text: &str| -> Vec<f32> {
        let seed = blake3::hash(text.as_bytes());
        (0..384)
            .map(|index| f32::from(seed.as_bytes()[index % 32]) / 255.0)
            .collect()
    };
    let storage = tempfile::tempdir().unwrap();
    let runtime = open(storage.path(), "memory", &root);
    let started = Instant::now();
    let mut texts = 0usize;
    loop {
        let report = runtime
            .refresh(
                FillBudget {
                    max_files: 100_000,
                    ..FillBudget::default()
                },
                &mut |batch: Vec<String>| {
                    texts += batch.len();
                    Ok(batch.iter().map(|text| wide(text)).collect())
                },
            )
            .unwrap();
        if report.deferred == 0 {
            break;
        }
    }
    let fill_ms = started.elapsed().as_millis();
    let _ = runtime.search(&wide("query"), 10, &|_| true).unwrap();
    let memory = runtime.memory();
    let files = ConfiguredMembershipWalker.files(&root).unwrap();
    let legacy = SemanticIndex::build(
        &root,
        &files,
        &mut |batch: Vec<String>| Ok(batch.iter().map(|text| wide(text)).collect()),
        64,
    )
    .unwrap();
    println!(
        "semantic memory root={} texts={} fill_ms={} view_family_arena_bytes={} view_private_bytes={} legacy_index_bytes={}",
        root.display(),
        texts,
        fill_ms,
        memory.family_arena,
        memory.private,
        legacy.estimated_memory().estimated_bytes.unwrap_or(0)
    );
}
