# Cargo test reply-cap regression

`command.sh`, `stdout`, `stderr`, and `exit` are byte-for-byte copies of the
reported SYNAPSE transcript. The command exited 101. The inputs are not trimmed:
libtest's three failures are near the end of stdout, followed in the registry's
merged display by 18,875 bytes of Cargo stderr (mostly progress and warnings).

## Exact agent-visible output

- Before: [`before.foreground.txt`](before.foreground.txt) and
  [`before.completion.txt`](before.completion.txt).
- After: [`after.foreground.txt`](after.foreground.txt) and
  [`after.completion.txt`](after.completion.txt).

These are the complete output text fields, including recovery markers and the
`shown N of M lines (cap)` trailer. Only the newly allocated capture artifact
paths are replaced with `<stdout>` and `<stderr>`; paths *inside* the original
transcript are unchanged. The foreground text comes from the terminal registry
snapshot used by bash's foreground reply. The completion text comes from an
actual emitted `BashCompleted` frame, not a separately simulated cap.

The before foreground text has none of the three failure status lines, none of
the three panic locations, and no `64 passed; 3 failed` aggregate. The before
completion text also loses those lines. Both still contain Cargo's rerun line:
that line was last in stderr. Afterward, both output fields contain all three
failure names, all three locations, all nonempty passing aggregates, the failed
aggregate, and the rerun instruction. Eight empty test targets are summarized
in one count line.

## Which layer dropped the evidence

`combine_streams` concatenates stdout, a newline, and stderr. The compound
command's top-level `&&` deliberately forces generic compression, because a
specific compressor must not delete the output of other commands in a shell
list. `compress_with_registry_exit_code` therefore returns generic ANSI-stripped,
consecutive-deduplicated output, still containing every failure and total. There
is no lossy compound-command split, Cargo extractor, or compressor tail cut in
this reproduction.

The terminal reply's **16 KiB byte cap** then kept the first 6 KiB of that merged
text and approximately the last 10 KiB. The final stdout failures fell in the
discarded middle; the tail consisted entirely of stderr. The exit-aware
completion preview subsequently capped the already-damaged text again. The
recovery marker correctly pointed to the captures, but did not preserve the
evidence in the agent's reply.

The fix reserves runner evidence before the reply and completion byte cuts and
spends the remaining budget on head/tail context. The context tail stays bounded
to twenty lines / 4 KiB and the command's last line is always kept. Protection is
shape-specific to libtest and nextest (`FAIL [`, `TIMEOUT [`, `Summary [`);
ordinary command caps are unchanged. Protection covers the first twenty
distinct panic location lines. All failing names, all nonempty totals, and rerun
instructions are mandatory: if those alone exceed the byte cap, the cap is soft
rather than silently hiding failures. The existing upstream compression-input
read limit is unchanged.

The Cargo extractor also retains a non-final rerun instruction when exit status
is unknown, so a wrapper's later progress cannot hide it before reply rendering.

## Tests

Run `cargo test -p agent-file-tools --lib -- cargo_verdict`.

- `cargo_verdict_fixture_golden_agent_output`: golden verdict block in the final
  foreground text and emitted completion frame, with both real byte caps active.
  Context length varies with platform-specific absolute recovery-path lengths;
  the complete verdict block is byte-identical on every platform.
- `cargo_verdict_fixture_keeps_failure_names`
- `cargo_verdict_fixture_keeps_panic_locations`
- `cargo_verdict_fixture_keeps_totals`
- `cargo_verdict_fixture_keeps_rerun_line` (also puts the rerun line in the middle)
- `cargo_verdict_fixture_layer_diagnosis`: proves the uncapped compressor retains
  the evidence and the previous byte-cap policy loses it.
- `cargo_verdict_nextest_keeps_fail_and_timeout_names`
- `cargo_verdict_nextest_keeps_timeout_name_independently`
- `cargo_verdict_nextest_keeps_panic_location`
- `cargo_verdict_nextest_keeps_summary`
- `cargo_verdict_passing_runs_keep_totals_and_collapse_empty_targets`
- `cargo_verdict_caps_do_not_change_other_command_output`
- `cargo_verdict_names_outgrow_soft_cap_but_panic_locations_are_bounded`
- `cargo_verdict_extractor_keeps_nonfinal_rerun_with_unknown_exit`

To update the exact output records intentionally, set `AFT_BASH_GOLDEN_CAPTURE`
to an absolute filename prefix within this fixture directory and run the fixture
tests. Normal tests do not write fixtures.
