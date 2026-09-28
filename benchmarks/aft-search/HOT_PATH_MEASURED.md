# Frozen-corpus dev work counts

Counts are the primary evidence; elapsed time is unoptimized and host-load-sensitive. The unchanged exact natural-language workload took 12.8 s in the old-code control and 45.8 s in the optimized binary, demonstrating why these timings cannot establish a production speedup. All measured ranked lane digests are byte-identical after normalizing only the temporary corpus-root prefix.

Corpora: AFT `840f9a0d3e28699bc5a4b37d49464cd7c86e833d`; opencode `5716f8ba60e79ec60ec485b6e5291c0b0bc1f252`. Before uses safe old-code controls (the original rank-to-discover/window algorithms, per-file matchers, per-chunk filtering and clone-before-removal), built with the same test profile and instrumented counters. Both binaries run on the exact same archived trees. Initial pre-optimization measurements independently identified these mechanisms.

## aft: real-corpus lanes

| Case | Allocations before → after | Requested bytes before → after | Posting reads | File reads | Dev ms before → after (ratio) |
|---|---:|---:|---:|---:|---:|
| lexical/natural_language | 142,440 → 1,160 | 37,322,698 → 853,796 | 184 → 92 | 0 → 0 | 1078.0 → 468.9 (0.43×) |
| exact/natural_language | 2,027,991 → 2,027,991 | 421,809,610 → 421,809,610 | 141 → 141 | 2122 → 2122 | 12794.4 → 45787.8 (3.58×) |
| lexical/code_literal | 21,082 → 1,615 | 5,571,618 → 625,312 | 28 → 14 | 0 → 0 | 353.6 → 6.1 (0.02×) |
| exact/code_literal | 1,514,953 → 935,789 | 383,800,415 → 369,825,262 | 24 → 24 | 1310 → 1310 | 15836.5 → 36912.1 (2.33×) |
| lexical/common_rare | 37,088 → 1,904 | 9,801,719 → 760,542 | 50 → 25 | 0 → 0 | 911.7 → 85.9 (0.09×) |
| exact/common_rare | 536,520 → 536,520 | 237,719,463 → 237,719,463 | 47 → 47 | 854 → 854 | 8813.0 → 27004.6 (3.06×) |
| anchored/search.*index | 6,639,591 → 78,369 | 2,399,579,927 → 108,821,599 | 0 → 0 | 3658 → 3658 | 18668.4 → 9403.0 (0.50×) |

## opencode: real-corpus lanes

| Case | Allocations before → after | Requested bytes before → after | Posting reads | File reads | Dev ms before → after (ratio) |
|---|---:|---:|---:|---:|---:|
| lexical/natural_language | 200,446 → 1,147 | 53,237,382 → 1,032,968 | 184 → 92 | 0 → 0 | 1235.3 → 63.3 (0.05×) |
| exact/natural_language | 2,592,583 → 2,592,583 | 395,901,009 → 395,901,009 | 51 → 51 | 2810 → 2810 | 9255.4 → 9912.0 (1.07×) |
| lexical/code_literal | 27,094 → 748 | 7,445,949 → 332,818 | 28 → 14 | 0 → 0 | 89.1 → 3.5 (0.04×) |
| exact/code_literal | 1,105,037 → 1,092,553 | 309,853,946 → 309,534,385 | 12 → 12 | 2169 → 2169 | 7803.8 → 7081.0 (0.91×) |
| lexical/common_rare | 45,751 → 1,049 | 12,296,265 → 506,298 | 50 → 25 | 0 → 0 | 243.4 → 4.5 (0.02×) |
| exact/common_rare | 449,122 → 449,122 | 160,285,169 → 160,285,169 | 8 → 8 | 729 → 729 | 2543.0 → 3422.6 (1.35×) |
| anchored/search.*index | 11,363,018 → 126,260 | 4,248,619,369 → 156,452,745 | 0 → 0 | 6860 → 6860 | 34754.2 → 3006.8 (0.09×) |

## Controlled workloads (same counts in both runs)

| Case | Allocations before → after | Requested bytes before → after | Removed work |
|---|---:|---:|---|
| lexical_exactness/distant | 12,445 → 427 | 495,918 → 124,101 | 3003 joined-window scans → 1002 once-only line scans |
| lexical_exactness/missing | 18,003 → 16 | 565,779 → 116,556 | 2997 window scans → 0 |
| semantic/many_chunks_per_file | 202 → 202 | 143,717 → 143,717 | unchanged: 8000 vector scores |
| semantic/refresh_1 | 1,913 → 1,832 | 453,560 → 384,294 | 16000 entry visits → 8000; 40 payload clones → 0 |
| semantic/refresh_100 | 189,257 → 181,256 | 45,440,032 → 38,515,248 | 16000 entry visits → 8000; 4000 payload clones → 0 |
| semantic/refresh_full_cap_100_deferred | 146,354 → 2,136 | 21,837,486 → 419,300 | 101 source reads → 1 |
| semantic/borrowed_many_chunks_per_file | 16,252 → 659 | 1,564,844 → 197,000 | 8000 path/filter calls → 200 |

AFT natural-language lexical score evaluations: **119344 → 496**; sort inputs: **119840 → 992**. Opencode: **169158 → 490** scores, **169648 → 980** sort inputs. Anchored literal regex compilations: AFT **3647 → 1**, opencode **6534 → 1**. Evaluated anchored windows: AFT **1008425 → 15**, opencode **1287002 → 0** (no matching literal windows). Source reads intentionally remain unchanged outside deferred semantic refresh.

The direct anchored workload treats `search.*index` as literal retained text. Public routed log-excerpt latency is measured separately. Requested bytes are cumulative allocator requests, not peak or retained heap. `hot-path-counts-*.json` retain every measured counter and timing.

## Standalone dev latency and public rendered-row parity

Two real corpora, isolated frozen source snapshots and empty storage. Only the health status bar and identical-call reminder suffixes are excluded; no ranked row, count/truncation message or result classification is removed. All **30 required-lane requests** match byte-for-byte. Three supplemental opencode regex requests also match; two of three AFT regex requests differ due to demonstrated baseline fallback instability (see `HOT_PATH_FALLBACK_INSTABILITY.md`).

| Corpus | Case | Median dev ms before → after | After/before | Ranked output |
|---|---|---:|---:|---|
| aft | natural_language | 48953.2 → 10569.0 | 0.22× | 3/3 identical |
| aft | identifier | 21275.4 → 6507.8 | 0.31× | 3/3 identical |
| aft | code_literal | 9968.6 → 1145.9 | 0.11× | 3/3 identical |
| aft | common_rare | 46360.3 → 4155.2 | 0.09× | 3/3 identical |
| aft | anchored | 60187.4 → 5361.2 | 0.09× | 3/3 identical |
| aft | regex | 106.9 → 106.5 | 1.00× | 1/3 identical |
| opencode | natural_language | 41268.2 → 15789.8 | 0.38× | 3/3 identical |
| opencode | identifier | 21609.7 → 2189.2 | 0.10× | 3/3 identical |
| opencode | code_literal | 18509.2 → 2066.5 | 0.11× | 3/3 identical |
| opencode | common_rare | 20202.6 → 3002.2 | 0.15× | 3/3 identical |
| opencode | anchored | 175696.5 → 13481.6 | 0.08× | 3/3 identical |
| opencode | regex | 740.8 → 211.1 | 0.28× | 3/3 identical |

Ratios compare the same dev profile, query plan and corpus. They are not claims about production latency: the before and after observations experienced different host load. Work-count reductions remain the primary evidence.

Exact recall: **16/16 ranked rows unchanged**, sentence rank-1 and pair recall@10 both **1.0**. Concept recall: **26/26 ranked rows unchanged**, hit@1 **0.5384615385**, hit@5 **0.7692307692**, MRR@10 **0.6394230769**. Full row evidence: `hot-path-recall-parity.json`.
