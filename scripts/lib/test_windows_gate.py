#!/usr/bin/env python3
"""Local, network-free checks for the Windows gate's plan and failure summary."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("windows_gate", Path(__file__).with_name("windows-gate.py"))
assert spec is not None and spec.loader is not None
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class WindowsGateTests(unittest.TestCase):
    def test_module_and_integration_slices(self):
        self.assertEqual(gate.select_tests([
            "crates/aft/src/bash_background/persistence.rs",
            "crates/aft/src/bash_background/registry.rs",
            "crates/aft/src/commands/read.rs",
            "crates/aft/tests/integration/bash_windows_test.rs",
            "docs/tools.md",
        ]), [
            {"kind": "lib", "target": "", "filter": "bash_background::"},
            {"kind": "lib", "target": "", "filter": "commands::"},
            {"kind": "test", "target": "integration", "filter": "bash_windows_test::"},
        ])

    def test_shared_infrastructure_requires_full_lib(self):
        for path in ["Cargo.toml", "Cargo.lock", "crates/aft/Cargo.toml", "crates/aft/build.rs",
                     "crates/aft/src/lib.rs", "crates/aft/src/context.rs", "crates/aft/src/config.rs",
                     "crates/aft/src/config_migrate.rs", "crates/aft/src/subc_config.rs",
                     "crates/aft/src/executor/mod.rs", "crates/aft/src/db/store.rs",
                     "crates/aft/src/db.rs", "crates/aft/src/executor.rs"]:
            with self.subTest(path=path):
                self.assertEqual(gate.select_tests([path, "crates/aft/src/commands/read.rs"]),
                                 [{"kind": "lib", "target": "", "filter": ""}])

    def test_full_and_explicit_filter(self):
        self.assertEqual(gate.select_tests([], full=True), [
            {"kind": "lib", "target": "", "filter": ""},
            {"kind": "test", "target": "integration", "filter": ""},
        ])
        self.assertEqual(gate.select_tests(["Cargo.lock"], test_filter="bash_background::persistence"),
                         [{"kind": "lib", "target": "", "filter": "bash_background::persistence"}])
        self.assertEqual(len(gate.select_tests([], full=True, test_filter="persistence")), 2)

    def test_other_harnesses_and_whole_target_subsumption(self):
        self.assertEqual(gate.select_tests([
            "crates/aft/tests/integration/main.rs", "crates/aft/tests/integration/bash_test.rs",
            "crates/aft/tests/engine/candidates_test.rs", "crates/aft/src/main.rs",
        ]), [
            {"kind": "bin", "target": "aft", "filter": ""},
            {"kind": "test", "target": "engine", "filter": "candidates_test::"},
            {"kind": "test", "target": "integration", "filter": ""},
        ])

    def test_cargo_arguments_do_not_exceed_vm_budget(self):
        self.assertEqual(gate.cargo_args({"kind": "test", "target": "integration", "filter": "bash_test::"}),
                         ["test", "--locked", "-j", "4", "-p", "agent-file-tools", "--test", "integration",
                          "bash_test::", "--", "--test-threads", "4"])

    def test_failure_summary_names_failures_and_panics(self):
        output = ["running 2 tests", "test good ... ok", "test windows_only ... FAILED", "failures:",
                  "---- windows_only stdout ----", "thread 'windows_only' panicked at src/lib.rs:2:1:",
                  "NON-VACUITY BREAK", "failures:", "    windows_only",
                  "test result: FAILED. 1 passed; 1 failed;", "error: test failed, to rerun pass `--lib`"]
        summary = gate.failure_summary(output)
        self.assertIn("windows_only", summary)
        self.assertIn("panicked at", summary)
        self.assertIn("NON-VACUITY BREAK", summary)
        self.assertNotIn("test good", summary)

    def test_lost_exit_status_cannot_pass(self):
        self.assertEqual(gate.gate_exit_code(0, ["GATE PASSED", "GATE FAILED"]), 1)
        self.assertEqual(gate.gate_exit_code(0, ["test result: ok. 10 passed;"]), 1)
        self.assertEqual(gate.gate_exit_code(1, ["GATE PASSED"]), 1)
        self.assertEqual(gate.gate_exit_code(0, ["GATE PASSED"]), 0)

    def test_non_rust_diff_is_explicitly_empty(self):
        self.assertEqual(gate.select_tests(["docs/test-executable-consolidation.md", "scripts/train-push.sh"]), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
