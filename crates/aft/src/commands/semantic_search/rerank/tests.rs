use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::onnx::{
    attention_batches_for_lengths, joint_rerank_threads, OnnxReranker, PairScorer, ScorerLoader,
};
use super::*;
use crate::commands::semantic_search::handle_semantic_search;
use crate::config::{Config, RerankConfig};
use crate::context::{AppContext, SemanticIndexStatus};
use crate::parser::TreeSitterProvider;
use crate::protocol::RawRequest;
use crate::search_index::SearchIndex;

thread_local! {
    static TEST_BACKEND: RefCell<Option<SelectedBackend>> = const { RefCell::new(None) };
}

/// The backend a test installed for its own thread, if any.
pub(super) fn test_backend_override() -> Option<SelectedBackend> {
    TEST_BACKEND.with(|slot| slot.borrow().clone())
}

/// Run `body` with `backend` serving every rerank on this thread.
fn with_backend<R>(
    backend: Arc<dyn RerankBackend>,
    fail_closed: bool,
    body: impl FnOnce() -> R,
) -> R {
    TEST_BACKEND.with(|slot| {
        *slot.borrow_mut() = Some(SelectedBackend {
            backend,
            fail_closed,
        })
    });
    let result = body();
    TEST_BACKEND.with(|slot| *slot.borrow_mut() = None);
    result
}

/// Scores a candidate by the number in its file name (`src/file_07.rs` scores
/// 7), so a reranked head is the reverse of name order. Can be told to fail a
/// number of first calls with a given error.
struct NumberedBackend {
    calls: AtomicUsize,
    failures: Mutex<Vec<RerankError>>,
    queries: Mutex<Vec<String>>,
}

impl NumberedBackend {
    fn new(failures: Vec<RerankError>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            failures: Mutex::new(failures),
            queries: Mutex::new(Vec::new()),
        })
    }
}

fn file_number(text: &str) -> f32 {
    let first_line = text.lines().next().unwrap_or_default();
    first_line
        .rsplit('_')
        .next()
        .and_then(|tail| tail.trim_end_matches(".rs").parse::<f32>().ok())
        .unwrap_or(-1.0)
}

impl RerankBackend for NumberedBackend {
    fn fingerprint(&self) -> RerankFingerprint {
        RerankFingerprint {
            backend: "numbered",
            model: "by-file-number".to_string(),
            revision: "1".to_string(),
        }
    }

    fn max_batch(&self) -> usize {
        usize::MAX
    }

    fn score(
        &self,
        query: &str,
        docs: &[RerankDoc<'_>],
        _deadline: Instant,
    ) -> Result<Vec<f32>, RerankError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.queries.lock().unwrap().push(query.to_string());
        let mut failures = self.failures.lock().unwrap();
        if !failures.is_empty() {
            return Err(failures.remove(0));
        }
        Ok(docs.iter().map(|doc| file_number(doc.text)).collect())
    }
}

// ---- pure ordering rules -------------------------------------------------

#[test]
fn noop_backend_returns_one_score_per_doc() {
    let backend = NoopRerankBackend;
    let docs = [RerankDoc { text: "a" }, RerankDoc { text: "b" }];
    let scores = backend
        .score("q", &docs, Instant::now() + Duration::from_secs(1))
        .expect("noop backend never fails");
    assert_eq!(scores, vec![0.0, 0.0]);
    assert_eq!(backend.fingerprint().backend, "noop");
}

#[test]
fn permutation_orders_by_score_within_segments_and_keeps_ties_stable() {
    // Two exact entries then three non-exact ones. The best non-exact score
    // must not climb above either exact entry.
    let segments = [1, 1, 0, 0, 0];
    let scores = [0.1, 0.9, 5.0, 2.0, 5.0];
    assert_eq!(permutation(&scores, &segments), vec![1, 0, 2, 4, 3]);
}

#[test]
fn permutation_uses_total_order_for_signed_zero() {
    // total_cmp puts +0.0 above -0.0, then the prior order breaks true ties.
    let segments = [0, 0, 0];
    assert_eq!(permutation(&[-0.0, 0.0, 0.0], &segments), vec![1, 2, 0]);
}

#[test]
fn settings_default_to_off_and_clamp_depth_and_timeout() {
    let settings = RerankSettings::resolve(&SearchConfig::default());
    assert_eq!(settings.backend, RerankBackendKind::Off);
    assert_eq!(settings.top_n, DEFAULT_TOP_N);
    assert_eq!(settings.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));

    let settings = RerankSettings::resolve(&SearchConfig {
        rerank: Some(RerankConfig {
            backend: Some(RerankBackendKind::Onnx),
            top_n: Some(10_000),
            timeout_ms: Some(1),
            ..RerankConfig::default()
        }),
    });
    assert_eq!(settings.top_n, MAX_TOP_N);
    assert_eq!(settings.timeout, Duration::from_millis(MIN_TIMEOUT_MS));
    let settings = RerankSettings::resolve(&SearchConfig {
        rerank: Some(RerankConfig {
            timeout_ms: Some(u64::MAX),
            top_n: Some(0),
            ..RerankConfig::default()
        }),
    });
    assert_eq!(settings.top_n, 1);
    assert_eq!(settings.timeout, Duration::from_millis(MAX_TIMEOUT_MS));
}

#[test]
fn joint_thread_budget_never_starves_or_oversubscribes() {
    // 16 cores: embedder 8, reranker 4.
    assert_eq!(joint_rerank_threads(8, 16, None), 4);
    // 4 cores: embedder 2, reranker 1.
    assert_eq!(joint_rerank_threads(2, 4, None), 1);
    // A 3-CPU quota on a 64-core host: embedder 2, reranker gets the 1 left.
    assert_eq!(joint_rerank_threads(2, 64, Some(3)), 1);
    // One core: the floor of one thread still applies.
    assert_eq!(joint_rerank_threads(1, 1, None), 1);
    // 32 cores: embedder capped at 8, reranker 4.
    assert_eq!(joint_rerank_threads(8, 32, None), 4);
}

#[test]
fn attention_batches_respect_the_budget_and_keep_order() {
    // 100² = 10k units per row; a 25k budget holds two rows.
    let batches = attention_batches_for_lengths(&[100, 100, 100, 100, 100], 25_000);
    assert_eq!(batches, vec![0..2, 2..4, 4..5]);
    // A single over-budget row still gets its own batch.
    let batches = attention_batches_for_lengths(&[10, 1_000, 10], 25_000);
    assert_eq!(batches, vec![0..1, 1..2, 2..3]);
}

// ---- candidate text ------------------------------------------------------

#[test]
fn candidate_text_is_bounded_relative_and_repeatable() {
    let project = tempfile::tempdir().unwrap();
    let file = project.path().join("src/long.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let mut source = String::from("fn header() {}\r\n\r\n");
    for line in 0..400 {
        source.push_str(&format!(
            "    let value_{line} = compute_the_answer({line});\r\n"
        ));
    }
    std::fs::write(&file, &source).unwrap();

    let whole = CandidateResult::new_exact(
        file.clone(),
        None,
        super::super::evidence_descriptor::EvidenceDescriptor::for_e1(1, true, false),
    );
    let first = candidate_text(project.path(), &whole, "compute_the_answer");
    let second = candidate_text(project.path(), &whole, "compute_the_answer");
    assert_eq!(first, second);
    assert!(
        first.len() <= CANDIDATE_TEXT_BUDGET_BYTES,
        "{}",
        first.len()
    );
    assert!(first.starts_with("src/long.rs\n"), "{first}");
    assert!(!first.contains('\r'));
    assert!(!first.contains("\n\n"), "blank lines are skipped");

    let symbol = CandidateResult::new_exact(
        file,
        Some(super::super::comparator::SymbolOffsetRange::new(0, 14)),
        super::super::evidence_descriptor::EvidenceDescriptor::for_e1(1, true, false),
    );
    assert_eq!(
        candidate_text(project.path(), &symbol, "anything"),
        "src/long.rs\nfn header() {}"
    );
}

// ---- served path ---------------------------------------------------------

fn page_request(query: &str, top_k: usize, offset: usize) -> RawRequest {
    serde_json::from_value(serde_json::json!({
        "id": "rerank-page-test",
        "command": "semantic_search",
        "query": query,
        "top_k": top_k,
        "offset": offset,
        "hint": "literal",
    }))
    .unwrap()
}

const PROBE: &str = "rerank_probe_token";

/// A lexical-only project with `count` files that all contain the probe.
fn probe_project(count: usize) -> (tempfile::TempDir, AppContext) {
    let project = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::new();
    for number in 0..count {
        let file = project.path().join(format!("src/file_{number:02}.rs"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let source = format!("pub fn {PROBE}_{number:02}() {{ /* {PROBE} */ }}\n");
        std::fs::write(&file, &source).unwrap();
        index.index_file(&file, source.as_bytes());
    }
    index.ready = true;
    let ctx = AppContext::new(
        Box::new(TreeSitterProvider::new()),
        Config {
            project_root: Some(project.path().to_path_buf()),
            ..Config::default()
        },
    );
    *ctx.search_index()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(index);
    *ctx.semantic_index_status()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = SemanticIndexStatus::Disabled;
    (project, ctx)
}

/// (file names of one page, the serialized response).
fn page(ctx: &AppContext, top_k: usize, offset: usize) -> (Vec<String>, String) {
    let response = handle_semantic_search(&page_request(PROBE, top_k, offset), ctx);
    assert!(response.success, "search failed: {response:?}");
    let value = serde_json::to_value(&response).unwrap();
    let files = value["results"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .map(|result| {
                    Path::new(result["file"].as_str().unwrap())
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        })
        .unwrap_or_default();
    (files, value.to_string())
}

fn stream(ctx: &AppContext, top_k: usize, total: usize) -> Vec<String> {
    let mut all = Vec::new();
    let mut offset = 0;
    while offset < total {
        let (files, _) = page(ctx, top_k, offset);
        if files.is_empty() {
            break;
        }
        all.extend(files);
        offset += top_k;
    }
    all
}

#[test]
fn served_stream_is_identical_across_page_sizes_and_offsets_around_the_head() {
    let (_project, ctx) = probe_project(40);
    let fused = stream(&ctx, 100, 40);
    assert_eq!(fused.len(), 40, "{fused:?}");

    let backend = NumberedBackend::new(Vec::new());
    with_backend(backend.clone(), false, || {
        let reference = stream(&ctx, 100, 40);
        assert_eq!(reference.len(), 40);

        // The head is the fused head, reordered by descending file number;
        // everything below the head keeps its fused position.
        let mut expected_head = fused[..DEFAULT_TOP_N].to_vec();
        expected_head.sort_by(|left, right| {
            file_number(&format!("src/{right}")).total_cmp(&file_number(&format!("src/{left}")))
        });
        assert_eq!(reference[..DEFAULT_TOP_N], expected_head[..]);
        assert_ne!(reference[..DEFAULT_TOP_N], fused[..DEFAULT_TOP_N]);
        assert_eq!(reference[DEFAULT_TOP_N..], fused[DEFAULT_TOP_N..]);

        for top_k in [10, 25, 50] {
            assert_eq!(stream(&ctx, top_k, 40), reference, "topK={top_k}");
        }
        for (top_k, offset) in [
            (5, 15),
            (5, 18),
            (3, 19),
            (2, 20),
            (5, 20),
            (5, 21),
            (1, 19),
        ] {
            let (files, text) = page(&ctx, top_k, offset);
            assert_eq!(
                files,
                reference[offset..offset + top_k],
                "topK={top_k} offset={offset}"
            );
            assert!(!text.contains("rerank skipped"));
        }
    });
    // Every page reused the order computed by the first request.
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.queries.lock().unwrap()[0], PROBE);
}

#[test]
fn a_committed_skip_is_replayed_even_after_the_backend_recovers() {
    let (_project, ctx) = probe_project(30);
    let fused = stream(&ctx, 100, 30);

    let backend = NumberedBackend::new(vec![RerankError::Unavailable("model loading".into())]);
    with_backend(backend.clone(), false, || {
        let (first, text) = page(&ctx, 10, 0);
        assert!(text.contains("rerank skipped: model loading"), "{text}");
        assert_eq!(first, fused[..10]);
        // The backend would now succeed; the page must still continue the
        // fused stream, with the same note.
        let (second, text) = page(&ctx, 10, 10);
        assert!(text.contains("rerank skipped: model loading"), "{text}");
        assert_eq!(second, fused[10..20]);
        assert_eq!(stream(&ctx, 25, 30), fused);
    });
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_committed_order_survives_a_later_backend_failure() {
    let (_project, ctx) = probe_project(30);
    // The first call succeeds and commits an order; later calls would time
    // out (the failures are queued below), but none should be made.
    let backend = NumberedBackend::new(Vec::new());
    let reference = with_backend(backend.clone(), false, || stream(&ctx, 100, 30));
    *backend.failures.lock().unwrap() = vec![RerankError::Timeout; 8];
    with_backend(backend.clone(), false, || {
        for top_k in [7, 10, 30] {
            assert_eq!(stream(&ctx, top_k, 30), reference);
        }
    });
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn default_config_never_reranks_or_notes() {
    let (_project, ctx) = probe_project(25);
    let (_, text) = page(&ctx, 25, 0);
    assert!(!text.contains("rerank skipped"), "{text}");
}

// ---- the head of one canonical list --------------------------------------

fn tiny_list(project: &Path, count: usize) -> CanonicalList {
    use super::super::blocks::{BlockEntry, CanonicalListKey, FrozenBlock};
    use super::super::evidence_descriptor::EvidenceDescriptor;
    let entries = (0..count)
        .map(|index| {
            let path = project.join(format!("src/file_{index:02}.rs"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("fn item_{index}() {{}}\n")).unwrap();
            BlockEntry {
                result: CandidateResult::new_exact(
                    path,
                    None,
                    EvidenceDescriptor::for_e1(1, true, false),
                ),
                tier_index: 0,
                admitted_contributions: Vec::new(),
                lane_attribution: Vec::new(),
                r3_order_index: index,
            }
        })
        .collect();
    CanonicalList {
        key: CanonicalListKey {
            project_root: project.to_path_buf(),
            snapshot_generation: "g1".to_string(),
            normalized_query: "q".to_string(),
            include_tests: false,
        },
        blocks: vec![FrozenBlock {
            tier_index: 0,
            entries,
        }],
    }
}

fn head_request<'a>(
    search: &'a SearchConfig,
    project: &'a Path,
    prose: Option<&'a str>,
    scope: Option<&'a HashSet<PathBuf>>,
) -> RerankRequest<'a> {
    RerankRequest {
        search,
        project_root: project,
        prose,
        path_scope: scope,
    }
}

fn names(list: &CanonicalList) -> Vec<String> {
    list.entries()
        .map(|entry| {
            entry
                .result
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn pattern_only_requests_are_never_reranked() {
    let project = tempfile::tempdir().unwrap();
    let mut list = tiny_list(project.path(), 5);
    let before = names(&list);
    let backend = NumberedBackend::new(Vec::new());
    let selected = Ok(SelectedBackend {
        backend: backend.clone(),
        fail_closed: false,
    });
    let search = SearchConfig::default();
    let settings = RerankSettings::resolve(&search);
    for prose in [None, Some(""), Some("   ")] {
        let outcome = rerank_block_zero(
            &mut list,
            &selected,
            &settings,
            &head_request(&search, project.path(), prose, None),
        )
        .unwrap();
        assert_eq!(outcome, HeadOutcome::NotApplicable);
    }
    assert_eq!(names(&list), before);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn path_scope_preference_is_kept_by_the_reorder() {
    let project = tempfile::tempdir().unwrap();
    let mut list = tiny_list(project.path(), 6);
    // Files 00..02 are in scope and lead; the backend prefers high numbers,
    // which are out of scope, so they may only reorder among themselves.
    let scope = (0..3)
        .map(|index| project.path().join(format!("src/file_{index:02}.rs")))
        .collect::<HashSet<_>>();
    let selected = Ok(SelectedBackend {
        backend: NumberedBackend::new(Vec::new()),
        fail_closed: false,
    });
    let search = SearchConfig::default();
    let settings = RerankSettings::resolve(&search);
    rerank_block_zero(
        &mut list,
        &selected,
        &settings,
        &head_request(&search, project.path(), Some("scoped"), Some(&scope)),
    )
    .unwrap();
    assert_eq!(
        names(&list),
        [
            "file_02.rs",
            "file_01.rs",
            "file_00.rs",
            "file_05.rs",
            "file_04.rs",
            "file_03.rs"
        ]
    );
    let indices = list
        .entries()
        .map(|entry| entry.r3_order_index)
        .collect::<Vec<_>>();
    assert_eq!(indices, (0..6).collect::<Vec<_>>());
}

#[test]
fn a_fail_closed_backend_turns_a_miss_into_a_search_error() {
    let project = tempfile::tempdir().unwrap();
    let mut list = tiny_list(project.path(), 4);
    let pack = fixture::FixturePack::empty(&RerankFingerprint {
        backend: "onnx",
        model: "bge-reranker-base".to_string(),
        revision: "r".to_string(),
    });
    let selected = Ok(SelectedBackend {
        backend: Arc::new(fixture::FixtureBackend::from_pack(&pack)),
        fail_closed: true,
    });
    let search = SearchConfig::default();
    let settings = RerankSettings::resolve(&search);
    let error = rerank_block_zero(
        &mut list,
        &selected,
        &settings,
        &head_request(&search, project.path(), Some("fixture miss"), None),
    )
    .expect_err("a fixture miss must fail the search");
    assert!(error.contains("no recorded score"), "{error}");
}

#[test]
fn recorder_captures_scores_that_the_fixture_backend_then_replays() {
    let project = tempfile::tempdir().unwrap();
    let pack_path = project.path().join("pack.json");
    let live = NumberedBackend::new(Vec::new());
    let recorder = fixture::recording(live.clone(), pack_path.clone());
    let search = SearchConfig::default();
    let settings = RerankSettings::resolve(&search);

    let mut recorded_list = tiny_list(project.path(), 5);
    rerank_block_zero(
        &mut recorded_list,
        &Ok(SelectedBackend {
            backend: recorder,
            fail_closed: false,
        }),
        &settings,
        &head_request(&search, project.path(), Some("record me"), None),
    )
    .unwrap();

    let pack = fixture::FixturePack::load(&pack_path).unwrap();
    assert_eq!(pack.scores.len(), 5);
    assert_eq!(pack.fingerprint.backend, "numbered");
    assert_eq!(pack.text_policy, TEXT_POLICY_REVISION);

    // Replaying the recorded scores from the pack gives the same order without
    // calling the live backend.
    let mut replayed_list = tiny_list(project.path(), 5);
    replayed_list.key.snapshot_generation = "g2".to_string();
    rerank_block_zero(
        &mut replayed_list,
        &Ok(SelectedBackend {
            backend: Arc::new(fixture::FixtureBackend::from_pack(&pack)),
            fail_closed: true,
        }),
        &settings,
        &head_request(&search, project.path(), Some("record me"), None),
    )
    .unwrap();
    assert_eq!(names(&replayed_list), names(&recorded_list));
    assert_eq!(live.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn unsupported_backend_choices_skip_with_a_reason() {
    for (backend, reason) in [
        (RerankBackendKind::Remote, "remote backend unavailable"),
        (RerankBackendKind::Synapse, "synapse backend unavailable"),
    ] {
        let search = SearchConfig {
            rerank: Some(RerankConfig {
                backend: Some(backend),
                ..RerankConfig::default()
            }),
        };
        let selected = select_backend(&search).expect("a configured backend is selected");
        assert_eq!(selected.err().as_deref(), Some(reason));
    }
    let search = SearchConfig {
        rerank: Some(RerankConfig {
            backend: Some(RerankBackendKind::Onnx),
            model: Some("some-other-model".to_string()),
            ..RerankConfig::default()
        }),
    };
    assert!(select_backend(&search)
        .unwrap()
        .err()
        .unwrap()
        .contains("unsupported rerank model"));
    assert!(select_backend(&SearchConfig::default()).is_none());
}

// ---- ONNX worker lifecycle (fake scorer) -------------------------------

struct CountingScorer {
    computed: Arc<AtomicUsize>,
    delay: Duration,
}

impl PairScorer for CountingScorer {
    fn score_pairs(
        &mut self,
        _query: &str,
        docs: &[String],
        _deadline: Instant,
    ) -> Result<Vec<f32>, RerankError> {
        std::thread::sleep(self.delay);
        self.computed.fetch_add(1, Ordering::SeqCst);
        Ok(docs.iter().map(|doc| doc.len() as f32).collect())
    }
}

fn fake_onnx(delay: Duration, loads: Arc<AtomicUsize>, computed: Arc<AtomicUsize>) -> OnnxReranker {
    let loader: ScorerLoader = Arc::new(move || {
        loads.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountingScorer {
            computed: computed.clone(),
            delay,
        }) as Box<dyn PairScorer>)
    });
    OnnxReranker::with_loader(
        RerankFingerprint {
            backend: "onnx",
            model: "fake".to_string(),
            revision: "0".to_string(),
        },
        loader,
    )
}

fn wait_until_ready(backend: &OnnxReranker) {
    let docs = [RerankDoc { text: "x" }];
    for _ in 0..200 {
        match backend.score("q", &docs, Instant::now() + Duration::from_secs(2)) {
            Err(RerankError::Unavailable(reason)) if reason == "model loading" => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return,
        }
    }
    panic!("the fake model never became ready");
}

#[test]
fn onnx_first_use_starts_loading_off_the_request_and_skips() {
    let loads = Arc::new(AtomicUsize::new(0));
    let computed = Arc::new(AtomicUsize::new(0));
    let backend = fake_onnx(Duration::ZERO, loads.clone(), computed.clone());
    let docs = [RerankDoc { text: "ab" }];
    let started = Instant::now();
    let first = backend.score("q", &docs, Instant::now() + Duration::from_secs(5));
    assert_eq!(
        first,
        Err(RerankError::Unavailable("model loading".to_string()))
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    wait_until_ready(&backend);
    assert_eq!(
        backend.score("q", &docs, Instant::now() + Duration::from_secs(5)),
        Ok(vec![2.0])
    );
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[test]
fn onnx_worker_drops_a_request_whose_deadline_passed_and_refuses_while_busy() {
    let loads = Arc::new(AtomicUsize::new(0));
    let computed = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(fake_onnx(
        Duration::from_millis(300),
        loads,
        computed.clone(),
    ));
    wait_until_ready(&backend);
    let baseline = computed.load(Ordering::SeqCst);
    let docs = [RerankDoc { text: "abc" }];

    // A slow job occupies the worker; a second request is refused as busy
    // instead of queuing behind it.
    let slow = {
        let backend = backend.clone();
        std::thread::spawn(move || {
            let docs = [RerankDoc { text: "abc" }];
            backend.score("q", &docs, Instant::now() + Duration::from_millis(50))
        })
    };
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        backend.score("q", &docs, Instant::now() + Duration::from_secs(5)),
        Err(RerankError::Unavailable("busy".to_string()))
    );
    // The caller of the slow job gave up at its deadline.
    assert_eq!(slow.join().unwrap(), Err(RerankError::Timeout));

    // A request whose deadline has already passed when the worker receives it
    // is dropped without inference.
    std::thread::sleep(Duration::from_millis(400));
    let computed_before = computed.load(Ordering::SeqCst);
    assert_eq!(
        backend.score("q", &docs, Instant::now()),
        Err(RerankError::Timeout)
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(computed.load(Ordering::SeqCst), computed_before);
    assert!(computed_before > baseline);
}

/// Loads a real cross-encoder from `AFT_RERANK_ONNX_TEST_MODEL_DIR` (a
/// directory with `onnx/model.onnx` or `model.onnx`, and `tokenizer.json`),
/// scores 20 query/document pairs, and prints the latency. Skipped when the
/// variable is unset, because no reranker model ships with the repository.
#[test]
fn real_onnx_model_scores_twenty_pairs_when_available() {
    let Some(dir) = std::env::var_os("AFT_RERANK_ONNX_TEST_MODEL_DIR").map(PathBuf::from) else {
        eprintln!("skipped: AFT_RERANK_ONNX_TEST_MODEL_DIR is not set");
        return;
    };
    let model = [dir.join("onnx/model.onnx"), dir.join("model.onnx")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("model.onnx under the test model directory");
    let files = onnx::ModelFiles {
        model,
        tokenizer: dir.join("tokenizer.json"),
    };
    let threads = onnx::rerank_intra_threads();
    let mut scorer =
        onnx::OnnxPairScorer::load("test-model", &files, threads).expect("load test model");
    let query = "where is the session cache invalidated";
    let mut docs = (0..19)
        .map(|index| {
            format!(
                "src/render/widget_{index}.rs\npub fn draw_widget_{index}(frame: &mut Frame) {{\n    frame.fill(Color::rgb({index}, 0, 0));\n    frame.stroke_border();\n}}"
            )
        })
        .collect::<Vec<_>>();
    docs.push(
        "src/session/cache.rs\npub fn invalidate(&mut self, id: SessionId) {\n    // Drop the cached session so the next lookup reloads it.\n    self.entries.remove(&id);\n}"
            .to_string(),
    );
    // Run once untimed (the first run allocates buffers), then time a second.
    scorer
        .score_pairs(query, &docs, Instant::now() + Duration::from_secs(60))
        .unwrap();
    let started = Instant::now();
    let scores = scorer
        .score_pairs(query, &docs, Instant::now() + Duration::from_secs(60))
        .unwrap();
    let elapsed = started.elapsed();
    eprintln!(
        "real rerank model: 20 pairs in {} ms with {threads} intra-op threads",
        elapsed.as_millis()
    );
    assert_eq!(scores.len(), 20);
    let best = scores
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .unwrap()
        .0;
    assert_eq!(
        best, 19,
        "the session-cache document should score highest: {scores:?}"
    );

    // Twenty candidates filled to the candidate-text byte budget, the longest
    // text the engine builds.
    let long_docs = (0..20)
        .map(|index| {
            let mut doc = format!("src/module_{index}.rs\n");
            while doc.len() < CANDIDATE_TEXT_BUDGET_BYTES - 40 {
                doc.push_str(&format!(
                    "    let value_{index} = compute(state, {index});\n"
                ));
            }
            doc
        })
        .collect::<Vec<_>>();
    let started = Instant::now();
    let scores = scorer
        .score_pairs(query, &long_docs, Instant::now() + Duration::from_secs(60))
        .unwrap();
    eprintln!(
        "real rerank model: 20 budget-sized pairs in {} ms",
        started.elapsed().as_millis()
    );
    assert_eq!(scores.len(), 20);
}
