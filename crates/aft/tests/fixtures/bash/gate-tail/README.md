# Multi-phase gate tail regression

`command.sh` models 327 lines without compiling anything: two libtest phases,
one nextest phase, stderr build progress interleaved with the phases, and a final
stdout verdict, `GATE PASSED: all phases green`. Its shape is modeled on a Rust
gate that announces each phase and checks generated files after testing.

The unit test generates the same stdout and stderr in-process, so it does not
require a working POSIX shell on Windows. AFT concatenates stdout followed by stderr
for compression, so the last line of that display is a stderr `Finished` line,
not the script's actual final stdout verdict. Cargo's output-shape extractor
already retained the display's last line and therefore believed its tail was
complete. Before the fix, the disk-backed terminal registry rendered:

```text
running 75 tests
test result: ok. 75 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
running 75 tests
test result: ok. 75 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.00s
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.00s
shown 6 of 327 lines (cap)
```

The verdict was absent before any reply byte cap ran. The fix preserves the
stdout/stderr boundary during the bounded capture read, restores missing stream
endings after **any** compressor tier, and reserves each stream's final line
before both the reply and completion byte caps. Stream order is not chronology;
neither capture's ending can safely be discarded in favor of the other. Final
lines are whole even when they exceed the byte budget. Existing zero-test-target
summaries remain collapsed into their equivalent counted summary.

Run `cargo test -p agent-file-tools --lib -- gate_tail` with isolated HOME/XDG
directories. The tests read the modeled fixture's stdout/stderr through the
disk-backed terminal registry and check both foreground output and an emitted
`BashCompleted` frame. They cover Cargo's command and output-shape extractors,
generic nextest/shell-list paths, package-manager and TOML tiers, padded output
that reaches the reply cap, and oversized UTF-8 final lines from both streams.
The fixture test failed before implementation with `script verdict was lost`.
