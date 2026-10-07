# Published exec-remote/v1 vectors

`published-v0.2.0.json` embeds every outcome (26) and unary reply (16) from
`cortexkit-exec-remote-types` 0.2.0's `test-vectors/exec-remote-v1` corpus.
Each entry preserves the original `.jcs` bytes as a JSON string and the original
`.sha256` digest. The fixture is MIT-licensed, like the published crate.

Source: https://static.crates.io/crates/cortexkit-exec-remote-types/cortexkit-exec-remote-types-0.2.0.crate
Archive SHA-256: `7d434715e82bfd47c960c54b4c3d9f6ab9fbba07b94e019dc3cf4787fe1d0f17`.

The 0.2.1 dependency adds `Accepted.env_not_forwarded`. All 26 outcomes and 16
replies, their case names, canonical bytes and original digests were checked
against the published 0.2.1 package and are unchanged, so this fixture retains
its original filename and bytes. The new accepted-frame vector is pinned
separately in `src/exec_remote/fixtures/frames/`; its provenance is documented
in the adjacent `SOURCE.md`.

When updating the contract dependency again, check the full published corpus,
including future-tag cases, and grade every added case explicitly in
`src/exec_remote/tests.rs`. Do not regenerate digests from modified goldens:
they are the published integrity checks. Embedding the corpus avoids running
Cargo or reading its registry cache at test runtime. The tests embed `Cargo.lock`
as well and check that exactly one contract version, 0.2.1, is locked, so a
dependency update requires an explicit corpus review rather than silently
continuing to grade old vectors.
