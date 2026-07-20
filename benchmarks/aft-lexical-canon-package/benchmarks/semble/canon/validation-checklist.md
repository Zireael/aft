# Lexical canon validation checklist

Before treating this canon as an oracle:

1. Check out every pinned repo revision from `repos.json`.
2. Verify every `relevant[].path` exists relative to the repo root.
3. Verify every `symbol` appears in the file or is a documented re-export.
4. For rows with `start_line`/`end_line`, verify the span still contains the expected symbol or method.
5. For structural rows, run `ast_search` manually and adjust `ast_pattern` syntax if AFT's ast-grep wrapper requires a different shape.
6. Move validated rows from `review_status: "seed"` to `review_status: "reviewed"`.
7. Do not move `unverified-seeds.json` rows into scored suites until repo revisions and relevance are pinned.
8. Add CI only after the denominator bug is fixed: empty/error attempts must count as zero.
