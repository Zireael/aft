# Views soak parity divergence `search / DataWordmark / line 9`: probe transport artifact

## Verdict

The divergence that `summary.md` reports for opencode (`divergent(search, DataWordmark, line 9)`,
second row of `probe-0f3900af641f5248.jsonl`, observed 2026-09-12T09:54:43Z) is **not a views
defect and not an AFT defect**. The two AFT outputs are identical. The views-on side was read
through `subc-probe`, which parses the tool response with `serde_json`'s default float parser. That
parser is not correctly rounded, and it moved five scores by one unit in the last place (ULP) of an
`f64`. Python's `json` parses the views-off side and rounds correctly, so that side keeps the exact
values AFT sent.

Root cause: `subconscious/crates/subc-core/src/bin/subc-probe.rs:386`,
`serde_json::from_slice(&frame.body)` into a `serde_json::Value`, re-printed at `:243` with
`serde_json::to_string_pretty`. No crate in the `subconscious` repository enables the `serde_json`
feature `float_roundtrip`, which makes that parse exact.

## What differs

Both responses contain the same 20 results in the same order with the same files, snippets, kinds,
locations and text rendering. Only two numeric fields differ: `lexical_score` and, on the
lexical-only rows where `score == lexical_score`, `score`. Every difference is exactly 1 ULP.

| baseline (views-off, NDJSON → Python `json`) | views-on (daemon → `subc-probe`) | ULP |
|---|---|---:|
| 1.1534698009490967 | 1.153469800949097 | +1 |
| 1.1761680841445923 | 1.1761680841445925 | +1 |
| 1.1722670793533325 | 1.1722670793533323 | −1 |
| 0.9503887295722961 | 0.950388729572296 | −1 |
| 0.9443580508232117 | 0.9443580508232116 | −1 |

Every baseline value is an exact `f32` widened to `f64`. `lexical_score` is `Option<f32>`
(`crates/aft/src/commands/semantic_search/mod.rs:212`), and AFT writes the response data with
`serde_json::to_writer` on both transports (`crates/aft/src/subc/wire.rs:526-623` for subc). None
of the views-on values can be represented as an `f32`, so they cannot have come from AFT's scoring.

## Reproduction (isolated, no daemon)

A scratch crate pinned to the workspace's `serde_json = "=1.0.149"` (with `preserve_order`, as in
`crates/aft`) parsed each baseline string with `serde_json::from_str::<Value>` and re-serialized
it. It also parsed the same string with Rust's `str::parse::<f64>`, which rounds correctly:

- The five baseline values above came back as **exactly** the five views-on values.
- The other eight distinct scores in the response (for example `1.1532834768295288`,
  `1.053737759590149` and `1.0026136636734009`) came back unchanged. That matches the probe, where
  those fields did not differ.
- `str::parse::<f64>` returned every input unchanged.

The router between AFT and the client does not touch the bytes. The daemon forwards frame bodies
as opaque bytes (`subconscious/crates/subc-daemon/src/lib.rs:3-5`) and only rewrites the header
(`router.rs:913-925`). The probe's own `canonical_output`
(`scripts/lib/views-soak/common.py:920-922`) compares numbers by their exact `repr`, so a 1-ULP
change is reported as a divergence.

## Why earlier readings were different

The first row of the same JSONL (08:05:33Z, `divergent(callgraph, githubFetch, line 2)`) used
the non-owned `baseline` checkout, whose callgraph answered `symbol_not_found`. That is the same
readiness confound that Run 1 in `branch-drill.md` documents. The probe was then moved to the
owned baseline (`baseline-owned`). With that baseline, the only remaining divergence is the float
transport artifact above.

## Consequences and fixes (not applied: out of scope for this phase)

- Real plugin clients are not affected: the TypeScript bridge uses `JSON.parse`, which rounds
  correctly. Any **Rust** client that parses AFT responses with default `serde_json` sees the same
  1-ULP shifts. Scores are not compared for equality across processes, so this has no product
  impact beyond diagnostics.
- Probe fix (AFT repo, `scripts/lib/views-soak/`): canonicalize floats before comparing, for
  example by rounding to the nearest `f32`. Alternatively, read the views-on side through a
  client that parses exactly.
- Transport fix (owner: subconscious): enable `serde_json/float_roundtrip` for `subc-core`, or
  have `subc-probe` print the raw frame body instead of re-serializing it.
- The parity summary should be regenerated after the probe fix. Do not hand-edit it: it is
  generated output.
