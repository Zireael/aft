#!/usr/bin/env python3
"""Small regression tests for corpus isolation and result comparison."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from run_hot_path import isolated_corpus

HERE = Path(__file__).resolve().parent


class HotPathHarnessTests(unittest.TestCase):
    def test_snapshot_uses_pinned_commit_not_dirty_worktree(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "source.rs").write_text("fn original() {}\n")
            subprocess.run(["git", "-C", str(root), "add", "source.rs"], check=True)
            subprocess.run(["git", "-C", str(root), "-c", "user.name=Harness", "-c", "user.email=harness@example.invalid", "commit", "-qm", "fixture"], check=True)
            commit = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
            (root / "source.rs").write_text("fn changed() {}\n")
            with isolated_corpus(root.resolve(), commit) as (snapshot, actual_commit):
                self.assertEqual(actual_commit, commit)
                self.assertEqual((snapshot / "source.rs").read_text(), "fn original() {}\n")
                self.assertFalse((snapshot / ".git").exists())

    def test_comparison_rejects_changed_rendered_row(self):
        report = {"complete": True, "rows": [{
            "corpus": "fixture", "revision": "abc", "query": "query", "repeat": 0,
            "case": "literal", "latency_ms": 10,
            "response": {"id": "1", "success": True, "status": "ready", "complete": True, "text": "a.rs\nb.rs"},
        }]}
        with tempfile.TemporaryDirectory() as temporary:
            before, after = (Path(temporary) / name for name in ("before.json", "after.json"))
            before.write_text(json.dumps(report))
            report["rows"][0]["response"]["id"] = "200"
            after.write_text(json.dumps(report))
            command = [sys.executable, str(HERE / "compare_hot_path.py"), str(before), str(after)]
            same = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(same.returncode, 0, same.stderr)
            report["rows"][0]["response"]["text"] = "b.rs\na.rs"
            after.write_text(json.dumps(report))
            changed = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(changed.returncode, 0)
            self.assertIn("ranked rows changed", changed.stderr)


if __name__ == "__main__":
    unittest.main()
