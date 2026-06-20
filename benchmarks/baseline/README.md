# Benchmark Baseline

This directory contains schema fixtures and configuration inventories for the
AFT Retrieval Intelligence v1 benchmark work.

## Why no raw benchmark numbers are committed

Raw benchmark numbers (latency, recall, MRR, NDCG) are hardware-sensitive and
vary across machines, CPU loads, and thermal states. Committing them creates
false regression signals when CI runs on different hardware.

Instead, this baseline records:
- **Fixture schema**: the structure of benchmark inputs (`schema.json`)
- **Config inventory**: all configurable fields and their defaults
- **SOURCE-CONDITIONAL resolutions**: confirmed APIs, entry points, and flags

## What IS committed

| File | Purpose |
|------|---------|
| `schema-2026-06-18.json` | Fixture schema + config field inventory + source-conditional resolutions |
| `README.md` | This file |

## How to reproduce baseline numbers

```bash
cd benchmarks/semble
bun run pilot.ts --binary <path-to-aft-binary> --k 10 2>&1 | tee /tmp/baseline-run.json
```

The JSON output contains per-query metrics. Compare against your own run
rather than against committed numbers.

## Schema version

Current schema version: **1** (see `benchmarks/semble/schema.json`).
