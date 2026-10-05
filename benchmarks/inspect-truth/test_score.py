"""Offline regression fixtures for the inspect-truth collection and score gate."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

MODULE = importlib.util.spec_from_file_location("inspect_truth", Path(__file__).with_name("run.py"))
assert MODULE is not None and MODULE.loader is not None
harness = importlib.util.module_from_spec(MODULE)
MODULE.loader.exec_module(harness)


def row(symbol="X", category="dead_code"):
    return {"path": "src/a.rs", "symbol": symbol, "line": 1, "category": category}


class ScoreFixtures(unittest.TestCase):
    def setUp(self):
        target = harness.CHECKOUT_ROOT / "target"
        target.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=target)
        self.root = Path(self.temp.name)
        self.spec = {"name": "fixture", "commit": "pin", "languages": [
            {"language": "rust", "include_paths": ["src/"], "oracle": {}}]}
        self.before = self.output("before", [])
        self.after = self.output("after", [])
        self.judgments = self.root / "judgments"
        self.judgments.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def output(self, name, rows, oracle=None, summary=None):
        out = self.root / name
        out.mkdir(exist_ok=True)
        items = [{"file": r["path"], "symbol": r["symbol"], "line": r["line"]} for r in rows]
        (out / "aft.json").write_text(json.dumps({"commit": "pin", "items": {"dead_code": items},
            "project_summary": {"dead_code": summary or {"count": len(rows)}}}))
        (out / "oracle.json").write_text(json.dumps({"rust": {
            "buckets": {"dead_code": oracle or []}, "unused_files": []}}))
        return out

    def judge(self, symbol="X", side="aft", category="dead_code", after=None, filename="A.json"):
        entry = {"repo": "fixture", "category": category, "side": side, "path": "src/a.rs",
                 "symbol": symbol, "verdict": "aft_correct" if side == "aft" else "oracle_wrong",
                 "note": "Fixture row checked independently."}
        (self.judgments / filename).write_text(json.dumps({
            "after_outputs": {"fixture": str(after or self.after)}, "entries": [entry]}))

    def score(self, out=None):
        return harness.score_repo(self.root, self.spec, out=out or self.after,
            baseline_out=self.before, judgments_dir=self.judgments)

    def bucket(self, result):
        return next(b for b in result["buckets"] if b["category"] == "dead_code")

    def test_judgment_before_only(self):
        self.output("before", [row()])
        self.judge()
        result = self.score()
        self.assertEqual(result["judgments_unapplied"]["A.json"], 1)
        self.assertEqual(self.bucket(result)["aft_rows_judged"], 0)

    def test_judgment_after_only(self):
        self.output("after", [row()])
        self.judge()
        bucket = self.bucket(self.score())
        self.assertEqual((bucket["aft_rows_judged"], bucket["aft_rows_unjudged"]), (1, 0))

    def test_judgment_absent_from_both_errors(self):
        self.judge()
        with self.assertRaisesRegex(ValueError, "A.json.*absent"):
            self.score()

    def test_judgment_sequential_A_B(self):
        self.output("after", [row()])
        self.judge()
        self.score()
        later = self.output("B", [])
        result = self.score(later)
        self.assertEqual(result["judgments_unapplied"]["A.json"], 1)
        self.assertEqual(len(json.loads((self.judgments / "A.json").read_text())["entries"]), 1)

    def test_oracle_judgments_drop_rows_before_matching(self):
        self.output("after", [row()], oracle=[row()])
        self.judge(side="oracle")
        bucket = self.bucket(self.score())
        self.assertEqual((bucket["oracle_rows_judged_out"], bucket["oracle_count"], bucket["agreed"]), (1, 0, 0))

    def test_judgments_are_category_scoped(self):
        self.output("after", [row()], oracle=[row()])
        self.judge()
        entries = harness.load_judgments(self.spec, self.before, self.judgments)
        kept, judged_out, judged = harness.apply_judgments("unused_exports", [row()], [row()], entries)
        self.assertEqual((len(kept), judged_out, judged), (1, 0, 0))

    def test_language_unsupported_is_scoreable(self):
        self.output("after", [row()], oracle=[row()], summary={"count": 1, "complete": False,
            "gaps": [{"kind": "language_unsupported", "language": "python", "files": 3}]})
        bucket = self.bucket(self.score())
        self.assertEqual(bucket["status"], "scored")
        self.assertEqual(bucket["precision"], 1.0)
        self.assertEqual(bucket["languages_unsupported"], [{"language": "python", "files": 3}])

    def test_analysis_incomplete_blocks_scoring(self):
        self.output("after", [row()], oracle=[row()], summary={"count": 1, "complete": False,
            "gaps": [{"kind": "analysis_incomplete", "files": 3}]})
        bucket = self.bucket(self.score())
        self.assertEqual(bucket["status"], "unknown")
        self.assertIsNone(bucket["aft_count"])
        self.assertIsNone(bucket["precision"])

    def test_tier2_unavailable_and_terminal_unavailable_are_unknown(self):
        for summary in ({"status": "unavailable"}, {"count": 0, "gaps": [{"kind": "tier2_unavailable"}]}):
            with self.subTest(summary=summary):
                self.output("after", [], summary=summary)
                self.assertIsNone(self.bucket(self.score())["aft_count"])

    def test_cell_rule_defined_to_na_precision_only_fully_judged(self):
        before = {"precision": 0.5, "recall": None, "aft_count": 2, "aft_rows_judged": 2}
        after = {"precision": None, "recall": None, "aft_count": 0}
        self.assertEqual(harness.cell_changes(before, after), [])
        before["aft_rows_judged"] = 1
        self.assertIn("precision", harness.cell_changes(before, after))

    def test_cell_rule_recall_na_and_lowering_fail(self):
        self.assertEqual(harness.cell_changes({"precision": 0.8, "recall": 0.5},
            {"precision": 0.7, "recall": None, "aft_count": 0}), ["precision", "recall"])

    def test_no_baseline_is_new(self):
        self.spec["baseline"] = False
        result = harness.score_repo(self.root, self.spec, out=self.after, judgments_dir=self.judgments)
        self.assertEqual(result["baseline"], "new, no baseline")

    def test_baseline_truncated_scopes_excluded_from_both_sides(self):
        self.output("before", [row()], oracle=[row()])
        record = json.loads((self.before / "aft.json").read_text())
        record["truncated_scopes"] = [{"category": "dead_code", "path": "src/a.rs", "count": 134, "returned": 100}]
        (self.before / "aft.json").write_text(json.dumps(record))
        self.output("after", [row()], oracle=[row()])
        result = self.score()
        bucket = self.bucket(result)
        self.assertEqual((bucket["aft_count"], bucket["oracle_count"]), (0, 0))
        self.assertEqual(len(result["baseline_aft_truncated_scopes"]), 1)


class CollectionFixtures(unittest.TestCase):
    def response(self, items, total, envelope=None, summary=None):
        details = {"dead_code": items}
        if envelope is not None:
            details["dead_code_list_envelope"] = envelope
        return {"success": True, "summary": {"dead_code": summary or {"count": total}}, "details": details}

    def test_offset_paging_only_when_envelope_advertises_it(self):
        first = self.response([{"file": "a.rs", "symbol": "A", "line": 1}], 2,
            {"total": 2, "offset": 0, "next_offset": 1})
        second = self.response([{"file": "a.rs", "symbol": "B", "line": 2}], 2,
            {"total": 2, "offset": 1, "next_offset": None})
        session = Mock()
        session.inspect.side_effect = [first, second]
        self.assertEqual(len(harness.collect_category(session, "dead_code", [], {}, [])), 2)
        self.assertEqual(session.inspect.call_args.kwargs["offset"], 1)

    def test_unknown_offset_ignored_uses_scope_fallback(self):
        session = Mock()
        session.inspect.side_effect = [self.response([], 2, {"total": 2}),
            self.response([{"symbol": "A"}], 1), self.response([{"symbol": "B"}], 1)]
        rows = harness.collect_category(session, "dead_code", ["a.rs", "b.rs"], {}, [])
        self.assertEqual(len(rows), 2)
        self.assertTrue(all("offset" not in call.kwargs for call in session.inspect.call_args_list))

    def test_pending_retried_but_terminal_unavailable_not_retried(self):
        session = Mock()
        session.inspect.side_effect = [self.response([], None, summary={"status": "pending"}),
            self.response([], 0), self.response([], None, summary={"status": "unavailable"})]
        with patch.object(harness.time, "sleep"):
            ready = harness.wait_until_ready(session, 60)
            terminal = harness.wait_until_ready(session, 60)
        self.assertEqual(ready["summary"]["dead_code"]["count"], 0)
        self.assertEqual(terminal["summary"]["dead_code"]["status"], "unavailable")
        self.assertEqual(session.inspect.call_count, 3)

    def test_baseline_building_gap_is_retried(self):
        session = Mock()
        session.inspect.side_effect = [self.response([], None, summary={"unavailable": True,
            "gaps": [{"kind": "analysis_incomplete", "reason": "inspect_phase_timeout; builder_state=building"}]}),
            self.response([], 0)]
        with patch.object(harness.time, "sleep"):
            ready = harness.wait_until_ready(session, 60)
        self.assertEqual(ready["summary"]["dead_code"].get("count"), 0)

    def test_pagination_rejects_overlap_or_changed_total(self):
        for second in (
            self.response([{"file": "a.rs", "symbol": "A", "line": 1}], 2,
                {"total": 2, "offset": 1, "next_offset": None}),
            self.response([{"file": "a.rs", "symbol": "B", "line": 2}], 3,
                {"total": 3, "offset": 1, "next_offset": None}),
        ):
            with self.subTest(second=second):
                session = Mock()
                session.inspect.side_effect = [self.response([{"file": "a.rs", "symbol": "A", "line": 1}], 2,
                    {"total": 2, "offset": 0, "next_offset": 1}), second]
                with self.assertRaises(RuntimeError):
                    harness.collect_category(session, "dead_code", [], {}, [])


class CheckoutFixtures(unittest.TestCase):
    setUp = ScoreFixtures.setUp
    tearDown = ScoreFixtures.tearDown
    output = ScoreFixtures.output
    score = ScoreFixtures.score

    def test_local_checkout_restores_config_and_status_even_on_error(self):
        self.spec["local_checkout"] = True
        config = self.root / ".cortexkit" / "aft.jsonc"
        config.parent.mkdir()
        config.write_bytes(b"// Tracked configuration\n{}\n")
        before = config.read_bytes()
        with patch.object(harness, "CHECKOUT_ROOT", self.root), patch.object(harness, "git_output", return_value=" M run.py\n"):
            with self.assertRaisesRegex(RuntimeError, "collection failed"):
                with harness.local_hygiene(self.root, self.spec) as record:
                    config.write_bytes(b"overwritten")
                    raise RuntimeError("collection failed")
        self.assertEqual(config.read_bytes(), before)
        self.assertEqual(record["git_status_before"], record["git_status_after"])

    def test_local_checkout_does_not_hide_install_changes(self):
        self.spec["local_checkout"] = True
        with patch.object(harness, "CHECKOUT_ROOT", self.root), patch.object(harness, "git_output", side_effect=["", " M tracked.txt\n"]):
            with self.assertRaisesRegex(RuntimeError, "changed git status"):
                with harness.local_hygiene(self.root, self.spec):
                    pass

    def test_install_guard_is_per_language_directory(self):
        self.spec["languages"][0].update(install=["cargo", "unused"], install_cwd="rust")
        self.spec["languages"].append({"language": "typescript", "include_paths": ["src/"],
            "oracle": {}, "install": ["bun", "install"], "install_cwd": "ts"})
        (self.root / "fixture" / "rust" / "node_modules").mkdir(parents=True)
        with patch.object(harness, "run", return_value=(0, "", "", 0)) as runner:
            harness.install(self.root, self.spec)
        runner.assert_called_once_with(["bun", "install"], cwd=self.root / "fixture" / "ts", env=None)

    def test_local_checkout_pin_is_read_from_git(self):
        self.spec["local_checkout"] = True
        with patch.object(harness, "git_output", return_value="actual-head\n") as git:
            self.assertEqual(harness.corpus_pin(self.root, self.spec), "actual-head")
        git.assert_called_once_with(harness.CHECKOUT_ROOT, "rev-parse", "HEAD")

    def test_collections_preserve_baseline_and_use_fresh_storage_on_retry(self):
        session = Mock()
        session.configure.return_value = {"success": True}
        session.calls = 1
        session.inspect.return_value = {"success": True,
            "summary": {c: {"count": 0} for c in harness.AFT_CATEGORIES}, "details": {}}
        out = harness.raw_dir(self.root, self.spec)
        (self.root / "fixture").mkdir()
        with patch.object(harness, "corpus_pin", return_value="pin"), patch.object(harness, "tracked_files", return_value=[]), \
                patch.object(harness, "AftSession", return_value=session) as sessions:
            harness.collect_aft(self.root, self.spec, Path("aft"), 0)
            first = (out / "aft.json").read_bytes()
            harness.collect_aft(self.root, self.spec, Path("different-aft"), 0)
            self.assertEqual((out / "aft.json").read_bytes(), first)
            harness.collect_aft(self.root, self.spec, Path("aft"), 0, "aaaaaaaaa")
        self.assertEqual(sessions.call_count, 2)
        self.assertNotEqual(sessions.call_args_list[0].args[2], sessions.call_args_list[1].args[2])

    def test_one_bucket_set_per_language(self):
        self.spec["languages"].append({"language": "typescript", "include_paths": ["src/"], "oracle": {}})
        snapshot = json.loads((self.after / "oracle.json").read_text())
        snapshot["typescript"] = {"buckets": {"dead_code": []}}
        (self.after / "oracle.json").write_text(json.dumps(snapshot))
        self.assertEqual([b["language"] for b in self.score()["buckets"]], ["rust", "typescript"])


if __name__ == "__main__":
    unittest.main()
