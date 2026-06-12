# trace_data legacy CallGraph to CallgraphStore inventory

## Verdict

`crates/aft/src/commands/trace_data.rs` can cut over from the legacy in-memory `CallGraph` to `CallgraphStore` + `SymbolCache` **without persisted schema additions**.

The store already persists the only cross-hop fact `trace_data` needs from the legacy engine: each call ref's containing symbol, byte range, callee text, resolution status, target file/symbol, and edge provenance. The narrow cutover shape is to keep the existing source/AST value-flow walk, then match each visited call-expression byte range against store call refs for the current store node. `raw_payload` is not needed: arguments and assignment RHS text still come from the freshly parsed source, and unresolved calls retain `short_name`/`full_ref` in `refs`.

The only useful addition is an **API/view**, not a table: `call_at_position(node, byte_start, byte_end) -> Resolved|Unresolved|None`, backed by existing `refs`/`edges`. Persisted row impact: **0**. Until then, `outgoing_calls_of(node)` + `unresolved_calls_of(node)` and in-memory byte-range matching are sufficient.

## 1. Exact legacy-engine inventory

### Command-level dependencies

`handle_trace_data` is still hard-wired to the legacy engine:

- It borrows `ctx.callgraph()` and errors when the legacy slot is empty (`trace_data.rs:68-78`; `context.rs:1245-1248`).
- It validates the input path and rejects files outside `project_root` (`trace_data.rs:80-106`).
- It resolves the user symbol with `CallGraph::resolve_symbol_query` before tracing (`trace_data.rs:108-111`; implementation at `callgraph.rs:934-940`).
- It invokes `CallGraph::trace_data(file, symbol, expression, depth, max_files)` (`trace_data.rs:113-115`) and maps `ProjectTooLarge` specially (`trace_data.rs:120-122`).

Legacy result/data shapes touched by the command:

- `DataFlowHop { file, symbol, variable, line, flow_type, approximate }` (`callgraph.rs:523-538`). The comment allows `"assignment"`, `"parameter"`, or `"return"`, but the current implementation only emits assignment and parameter hops.
- `TraceDataResult { expression, origin_file, origin_symbol, hops, depth_limited }` (`callgraph.rs:540-554`).

### Legacy graph data shapes used during the walk

`trace_data` indirectly relies on these legacy `callgraph.rs` structures:

- `CallSite { callee_name, full_callee, line, byte_start, byte_end }` (`callgraph.rs:129-141`).
- `SymbolMeta { kind, exported, signature, line, range }` (`callgraph.rs:143-157`).
- `FileCallData { calls_by_symbol, exported_symbols, symbol_metadata, default_export_symbol, import_block, lang }` (`callgraph.rs:159-175`).
- `EdgeResolution::{Resolved { file, symbol }, Unresolved { callee_name }}` (`callgraph.rs:201-208`).

`build_file_data` builds those shapes by reading/parsing the file, parsing imports, extracting symbols, collecting call sites with byte ranges, exported symbols, metadata, and default-export metadata (`callgraph.rs:2853-3033`). `build_file` caches the result in the legacy `CallGraph.data` map (`callgraph.rs:922-931`).

### Walk setup and reparse behavior

`CallGraph::trace_data`:

1. Canonicalizes the input and records the relative origin path (`callgraph.rs:2126-2127`).
2. Builds legacy `FileCallData` and resolves the scoped symbol again (`callgraph.rs:2129-2133`; resolver candidate logic at `callgraph.rs:79-127`).
3. Enforces `max_callgraph_files` with `project_file_count_bounded` before walking (`callgraph.rs:2135-2144`).
4. Starts recursive `trace_data_inner` with a `(file, symbol, tracking_name)` visited set (`callgraph.rs:2146-2158`).

Every `trace_data_inner` visit reparses source regardless of legacy `build_file` cache:

- Reads the file from disk (`callgraph.rs:2191-2195`).
- Detects language, initializes a tree-sitter parser, and parses the source (`callgraph.rs:2197-2210`).
- Extracts symbols from that just-parsed tree (`callgraph.rs:2212-2216`).
- Finds the requested symbol by scoped identity or bare name (`callgraph.rs:2217-2223`).
- Converts the symbol range to bytes and finds the AST node covering that range (`callgraph.rs:2225-2235`; byte conversion helper at `edit.rs:16-23`).
- Walks that body with `tracked_names = [tracking_name]` (`callgraph.rs:2237-2255`).

This means the store does **not** need to persist assignment text, argument text, or function bodies. `SymbolCache` can cover the symbol table/range part; the AST body walk still needs current source.

### Hop A: same-body assignment frontier

`walk_for_data_flow` treats these AST kinds as assignment/declaration sites: `variable_declarator`, `assignment_expression`, `augmented_assignment_expression`, `assignment`, `let_declaration`, `short_var_declaration` (`callgraph.rs:2278-2287`). It delegates extraction to `extract_assignment_info` (`callgraph.rs:2289-2292`).

`extract_assignment_info` is textual/AST-local only:

- TS/JS `variable_declarator`: reads `name` and `value`; destructuring is approximate if the initializer contains a tracked name; otherwise it tracks exact RHS matches plus `tracked.property` / `tracked[index]` prefixes (`callgraph.rs:2378-2401`).
- TS/JS assignment expressions: tracks only when RHS text exactly equals a tracked name (`callgraph.rs:2404-2414`).
- Python `assignment`: same exact-RHS behavior (`callgraph.rs:2416-2426`).
- Rust/Go declarations: same exact-RHS behavior (`callgraph.rs:2428-2441`).

On exact assignment, it emits an `assignment` hop and appends the assigned name to `tracked_names` (`callgraph.rs:2293-2303`). On destructuring/pattern approximation, it emits one approximate assignment hop and returns from that subtree (`callgraph.rs:2305-2315`). No callgraph/store API is involved for this hop.

### Hop B: call argument -> callee parameter frontier

`walk_for_data_flow` checks `call_expression`, `call`, and `macro_invocation` nodes (`callgraph.rs:2320-2322`) with `check_call_for_data_flow`.

`check_call_for_data_flow` does the argument-position work from the reparsed AST:

- Finds an `arguments` or `argument_list` child (`callgraph.rs:2466-2473`).
- Iterates argument children, skipping punctuation (`callgraph.rs:2475-2493`).
- Emits approximate parameter hops for spread/splat arguments containing a tracked name (`callgraph.rs:2495-2505`).
- Records `(position, tracked_name)` when an argument text exactly equals a tracked name (`callgraph.rs:2514-2516`).
- Returns if no tracked argument position was found (`callgraph.rs:2525-2527`).
- Extracts callee text from the call node's `function` field (`callgraph.rs:2529-2538`; helper at `callgraph.rs:3339-3355`).

The legacy next-file/symbol frontier is then found with:

- `FileCallData.import_block` from `self.data[file]` (`callgraph.rs:2540-2546`).
- `resolve_cross_file_edge(full_callee, short_callee, file, import_block)` (`callgraph.rs:2548`; public wrapper at `callgraph.rs:942-962`).
- The resolver covers Rust module paths, namespace imports, named/default/aliased imports, barrel/index re-exports, and exported-symbol fallback (`callgraph.rs:765-920`).

For a resolved edge, `trace_data`:

1. Enforces depth (`callgraph.rs:2555-2558`).
2. Builds the target file again to get metadata (`callgraph.rs:2560-2567`).
3. Reads target `SymbolMeta.signature`, extracts parameter names with `extract_parameters`, and reads the target start line (`callgraph.rs:2568-2582`).
4. Emits a non-approximate `parameter` hop for each tracked argument position that has a corresponding parameter (`callgraph.rs:2584-2596`).
5. Recurses into `(target_file, target_symbol, param_name)` (`callgraph.rs:2598-2608`).

### Hop C: unresolved-callee handling

When the legacy resolver returns `Unresolved`, `trace_data` has a special same-file fallback:

- If `full_callee` is bare (`is_bare_callee`, `callgraph.rs:75-77`), it tries `resolve_symbol_query_in_data` against the current file's `FileCallData` (`callgraph.rs:2612-2619`).
- If that local symbol resolves, it extracts parameters from the current file metadata, emits parameter hops, and recurses into the same file (`callgraph.rs:2621-2660`).
- If there is no local symbol, it emits approximate `parameter` hops with `symbol = callee_name`, `variable = tracked`, and the call node's line, then stops (`callgraph.rs:2663-2674`).

The unresolved hop therefore needs only the call's byte position, callee short/full text, and whether the store already has a resolved local/import edge for that position.

## 2. Store/SymbolCache coverage by need

| Need from legacy walk | Existing store coverage | SymbolCache coverage | Gap? |
|---|---|---|---|
| Resolve user symbol query in a file | `CallGraphStore::nodes_for(file, symbol)` returns matching positional nodes (`callgraph_store/mod.rs:996-1006`); `nodes_for_file_matching_symbol` matches scoped or bare names (`callgraph_store/mod.rs:1375-1398`). The existing adapter already collapses positional nodes to legacy-style ambiguity by scoped symbol (`callgraph_store_adapter.rs:628-680`). | Cached `Symbol` has `name`, `range`, `signature`, `scope_chain`, `exported` (`symbols.rs:67-83`) and `FileParser::extract_symbols` returns cached symbols when fresh (`parser.rs:1271-1291`). | No schema gap. Reuse adapter-style symbol resolution for parity. |
| Locate the function body to walk | Store `nodes` persist selected symbol/name ranges, not body ranges: records use `selection_range` (`callgraph_store/mod.rs:2361-2364`), which narrows to the identifier span on the start line (`callgraph_store/mod.rs:2418-2425`). | SymbolCache stores full parser `Symbol.range` (`symbols.rs:67-75`); `SymbolCache::get` validates freshness and returns cached symbols (`parser.rs:917-923`). | Store alone is insufficient by design, but SymbolCache covers it. The value-flow AST still reparses current source. |
| Track assignments/destructuring/spread/argument text | Not persisted. Store schema has call/import refs, edges, dispatch hints, nodes, files, etc., but no assignment/value-ref table (`callgraph_store/mod.rs:1856-1971`). | Not persisted as refs; AST reparse covers it. | No schema addition needed if trace_data keeps reparsing. A `value_refs` table would be a new feature, not required for parity. |
| Map a call node at byte range P to target file/symbol | `refs` stores `caller_node`, `kind`, `short_name`, `full_ref`, `line`, `byte_start`, `byte_end`, `status`, `target_node`, `target_file`, `target_symbol`, `provenance` (`callgraph_store/mod.rs:1891-1912`). `edges` stores `ref_id`, `source_node`, `target_node`, `target_file`, `target_symbol`, `kind`, `line`, `provenance` and indexes `source_node/kind`, `target_file/symbol/kind`, `ref_id` (`callgraph_store/mod.rs:1925-1939`). `outgoing_calls_of(node)` exposes resolved calls with byte ranges and provenance (`callgraph_store/mod.rs:1099-1103`, `1567-1618`). | AST reparse supplies the call node byte range and argument position. | No schema gap. Use byte-range matching against `outgoing_calls_of(node)`. Optional API/view can make this one lookup. |
| Preserve unresolved-callee approximate hop | Unresolved call refs keep `COALESCE(short_name, full_ref, '')`, `full_ref`, `line`, `byte_start`, `byte_end` (`callgraph_store/mod.rs:1620-1647`). The refs table has those columns without any raw payload (`callgraph_store/mod.rs:1891-1912`). | AST reparse supplies the tracked argument variable and line if needed. | No schema gap. Match byte range against `unresolved_calls_of(node)` and emit the same approximate hop. |
| Legacy same-file bare fallback | Store build already resolves bare local calls through `resolve_local_target`, using the same `is_bare_callee` and `resolve_symbol_query_in_data` logic (`callgraph_store/mod.rs:3257-3276`). Such calls produce edges in `insert_resolved_ref` (`callgraph_store/mod.rs:3665-3683`). | SymbolCache can still provide target param names if needed. | No schema gap. Prefer the store edge; do not re-run the legacy fallback. |
| Target parameter names and target line | `StoreNode` carries `line`, `signature`, and `lang` (`callgraph_store/mod.rs:183-195`). Store nodes are populated from `SymbolMeta.signature` and kind/export data (`callgraph_store/mod.rs:2380-2398`). Existing `impact_of` proves parameter extraction from store node signature/lang works (`callgraph_store/mod.rs:1065-1070`). | SymbolCache can also provide full symbol signature/range. | No schema gap. Continue using `extract_parameters` until helper extraction moves it out of `callgraph.rs`. |
| Project-size/scanned-files guard | Store can report indexed file count (`callgraph_store/mod.rs:982-986`, `1350-1353`), but existing store-backed nav commands do not enforce legacy `max_callgraph_files`. | N/A | Behavior decision, not schema. For strict parity, check `indexed_file_count > max_callgraph_files` after store is ready and return `project_too_large`; otherwise follow the five store-backed ops and drop the request-time guard. |
| `raw_payload`-style call text | There is no `raw_payload` column in `refs`; the schema columns are explicit and lean (`callgraph_store/mod.rs:1891-1912`), and insertion writes only those columns (`callgraph_store/mod.rs:3636-3641`). | Reparse current source for raw text. | No gap for trace_data. Raw call/argument text is not needed from the DB. |

### Provenance parity caution

The store has supplemental method-dispatch edges (`name_match` / `type_match`) in addition to resolver edges (`treesitter+resolver`) (`callgraph_store/mod.rs:20-24`). `insert_method_dispatch_edge` writes those edges with supplemental provenance (`callgraph_store/mod.rs:3719-3731`, `3737-3760`). Legacy `trace_data` would usually treat such member calls as unresolved/approximate.

For byte parity, the trace_data cutover should initially treat only `site.resolved_by() == "treesitter+resolver"` as a resolved parameter frontier. `StoreCallSite::resolved_by` exposes provenance (`callgraph_store/mod.rs:215-217`), and `supplemental_resolution` identifies `name_match`/`type_match` (`callgraph_store/mod.rs:219-224`). Following supplemental edges can be a later enhancement, but doing it in the first cutover will change output shape/recursion.

## 3. Gaps and narrow additions

### Required persisted schema additions

None.

### Optional API/view: call lookup at byte range

Narrowest addition if the cutover wants a single store call instead of scanning per-symbol outgoing calls:

```rust
pub enum StoreForwardCallRef {
    Resolved(StoreCallSite),
    Unresolved(StoreUnresolvedCall),
}

pub fn call_at_position(
    &self,
    node: &StoreNode,
    byte_start: usize,
    byte_end: usize,
) -> Result<Option<StoreForwardCallRef>>
```

Implementation strategy on existing tables:

1. Resolved branch: query `edges e JOIN refs r ON r.ref_id = e.ref_id` where `e.kind = 'call'`, `e.source_node = node.id`, and `r.byte_start/r.byte_end` match. Existing indexes `idx_edges_source_kind` and `idx_edges_ref_id` cover the join path (`callgraph_store/mod.rs:1936-1939`).
2. Unresolved branch: query `refs` where `caller_node = node.id`, `kind = 'call'`, `status = 'unresolved'`, matching byte range, and no call edge exists; this is the same predicate `unresolved_calls_for_node` uses (`callgraph_store/mod.rs:1624-1633`). Existing `idx_refs_caller_node_kind` covers `caller_node/kind/status` (`callgraph_store/mod.rs:1913-1916`).
3. Return `None` when no persisted ref matches; the trace_data AST walker can then emit the legacy approximate unresolved hop or stop, depending on callee extraction.

Rows/size impact: **0 persisted rows, 0 table bytes**. If a future profile shows high outdegree symbols, a covering index on `(caller_node, kind, byte_start, byte_end)` could be added, but the initial in-memory scan is likely cheaper and avoids schema churn.

### Rejected addition: value_refs table

A `value_refs`/assignment table would store one or more rows per identifier/assignment/argument occurrence, i.e. O(identifier references) rather than O(call refs). That is larger than the existing `refs`/`edges` surface and unnecessary because trace_data already reparses current source to classify assignments, destructuring, spreads, and argument positions.

## 4. Recommended cutover shape

1. Add a store-backed trace_data adapter next to `callgraph_store_adapter.rs`, but keep the public JSON shape exactly `TraceDataResult`/`DataFlowHop`.
2. In `handle_trace_data`, mirror `call_tree`'s store access path: validate params/path, then call `ctx.callgraph_store_for_ops()` and map `Building`, `Unavailable`, and `Error` through the existing responses (`call_tree.rs:68-85`; response helpers at `callgraph_store_adapter.rs:577-626`).
3. Resolve the start symbol via `store.nodes_for` plus the existing collapse-by-scoped-symbol parity logic (`callgraph_store_adapter.rs:628-680`).
4. For each recursive visit, parse current source and use SymbolCache/full parser symbols to find the body range; do not use store node selection ranges as body ranges.
5. Walk assignments and argument positions with the existing AST logic. This can be extracted from `CallGraph` into neutral helpers during H2-c, but the cutover itself can copy/minimize logic first if review wants small risk.
6. For each tracked call argument, match the call node byte range to the current store node's calls:
   - First check resolver-provenance `outgoing_calls_of` matches and use target `StoreNode.signature/lang/line` to map argument position to parameter.
   - Then check `unresolved_calls_of` matches and emit legacy approximate parameter hops.
   - Ignore or approximate supplemental `name_match`/`type_match` edges in v1 for byte parity.
7. Recurse with the target `(file, symbol, param_name)` and keep the same `(file, symbol, tracking_name)` visited key/depth semantics.
8. Decide explicitly whether to preserve the legacy `max_callgraph_files` error using `indexed_file_count` or align trace_data with the five store-backed ops by relying on async store availability.

## 5. Test and parity strategy

The existing Phase-2a precedent is the byte-parity harness in `crates/aft/tests/callgraph_store_test.rs`:

- `store_op_outputs_match_legacy_for_tier1_languages` builds a store over a fixture project (`callgraph_store_test.rs:18-25`).
- `assert_op_parity` serializes legacy JSON and store JSON, then compares the exact bytes (`callgraph_store_test.rs:1196-1209`).
- The harness already snapshots the five store-backed nav ops: `callers`, `call_tree`, `impact`, `trace_to`, and `trace_to_symbol` (`callgraph_store_test.rs:1156-1193`), with explicit examples for TypeScript, JavaScript, and Rust (`callgraph_store_test.rs:27-150`, `152-285`, `287-340`).

The same harness pattern applies to trace_data. Add byte-parity cases that call legacy `graph.trace_data(...)` and the new store adapter for:

- Local assignment tracking from `data_flow.ts` (`callgraph_test.rs:2058-2117`; fixture at `data_flow.ts:3-7`).
- Cross-file parameter mapping from `processInput(cleaned)` to `input` (`callgraph_test.rs:2119-2186`; target fixture at `data_processor.ts:1-4`).
- Destructuring approximation (`callgraph_test.rs:2188-2227`; fixture at `data_flow.ts:9-12`).
- Same-file bare callee fallback, unresolved callee, spread/splat, and depth/cycle fixtures (new small unit fixtures if not already covered).
- A supplemental method-dispatch fixture proving `name_match`/`type_match` does not silently create non-legacy recursive hops in the parity mode.

Command-level integration tests should separately cover `callgraph_building`, `callgraph_unavailable` in a read-only worktree, and `callgraph_stale` if a stale-file path can be forced.

## 6. Mid-build and worktree-bridge risks

### Mid-build / `callgraph_building`

Current legacy `trace_data` can parse and build its in-memory graph synchronously once configured. A store-backed trace_data should inherit the five-nav-op behavior: `callgraph_store_for_ops` returns `Building` when a cold build is in flight or just started (`context.rs:1356-1466`), and `building_response` emits code `callgraph_building` (`callgraph_store_adapter.rs:601-610`). That is a user-visible transient behavior change unless documented/tested.

Also, trace_data reparses current source while store edges come from the persisted generation. If a file changes during/after build, byte ranges can fail to match or, worse, match an old call at the same byte range. The store path already has generation revalidation (`context.rs:1340-1354`) and replay of pending source paths after inline build wait (`context.rs:1436-1447`), but trace_data should still prefer a stale/error response over fabricating partial flow when store freshness is uncertain.

### Worktree bridge

`callgraph_store_for_ops` treats worktree bridges as read-only: it opens an existing store with `open_readonly` and never cold-builds in the worktree (`context.rs:1379-1392`; `open_readonly` at `callgraph_store/mod.rs:474-492`). If no main-checkout store exists, `unavailable_response` returns `callgraph_unavailable` with a worktree-specific message (`callgraph_store_adapter.rs:612-625`).

trace_data has an extra bridge risk beyond the existing five ops: it must reparse source for assignments/argument text. If it reparses the worktree file but resolves calls against a read-only store built for the main checkout, byte-range matching is only safe when file contents are identical. The cutover should either:

1. Read/parse from the same canonical root represented by the store, or
2. Verify freshness/content identity before matching and return `callgraph_stale`/`callgraph_unavailable` when the worktree differs.

This is not a schema blocker because the store already tracks file content hashes and freshness fields in `files`/`backend_file_state` (`callgraph_store/mod.rs:1856-1865`, `1957-1966`), but it is a parity and correctness risk that needs an integration test.
