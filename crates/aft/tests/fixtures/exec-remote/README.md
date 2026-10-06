# Published exec-remote/v1 vectors

`published-v0.2.0.json` embeds every outcome (26) and unary reply (16) from
`cortexkit-exec-remote-types` 0.2.0's `test-vectors/exec-remote-v1` corpus.
Each entry preserves the original `.jcs` bytes as a JSON string and the original
`.sha256` digest. The fixture is MIT-licensed, like the published crate.

Source: https://static.crates.io/crates/cortexkit-exec-remote-types/cortexkit-exec-remote-types-0.2.0.crate
Archive SHA-256: `7d434715e82bfd47c960c54b4c3d9f6ab9fbba07b94e019dc3cf4787fe1d0f17`.

When updating the contract dependency, refresh the full corpus from that
published version, including future-tag cases, and grade every added case
explicitly in `src/exec_remote/tests.rs`. Do not regenerate digests from modified
goldens: they are the published integrity checks. Embedding the corpus avoids
running Cargo or reading its registry cache at test runtime. The tests embed
`Cargo.lock` as well and check that exactly one contract version, 0.2.0, is locked,
so a dependency update requires an explicit corpus update rather than silently
continuing to grade old vectors.
