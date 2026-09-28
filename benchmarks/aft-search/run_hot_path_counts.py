#!/usr/bin/env python3
"""Run opt-in work counts on a frozen real corpus, never on changing source files."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess

from run_hot_path import isolated_corpus

ROOT = Path(__file__).resolve().parents[2]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, default=ROOT)
    parser.add_argument("--revision", default="HEAD")
    args = parser.parse_args()
    source = args.corpus.resolve()
    with isolated_corpus(source, args.revision) as (corpus, commit):
        print(f"corpus_source:{source}\ncorpus_commit:{commit}", flush=True)
        env = os.environ.copy()
        env.update(AFT_PERF_CORPUS=str(corpus), CARGO_BUILD_RUSTC_WRAPPER="", RUSTC_WRAPPER="")
        env.setdefault("CARGO_BUILD_JOBS", "2")
        subprocess.run([
            "cargo", "test", "-p", "agent-file-tools", "--lib", "hot_path",
            "--", "--ignored", "--nocapture", "--test-threads=1",
        ], cwd=ROOT, env=env, check=True)


if __name__ == "__main__":
    main()
