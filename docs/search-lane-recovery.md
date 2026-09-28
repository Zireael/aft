# Search lane recovery

## Incident evidence (2026-09-28)

The supplied daemon excerpt does not contain lane observations for the two
refusals, so it cannot prove the exact state of both lanes at refusal time.
It does establish:

- The owner's 21:00:43 bind queued its search reload behind the cold-build
  limiter; it acquired a slot at 21:02:29 after 105543 ms.
- Its semantic disk load returned 26728 entries at 21:00:53; semantic refresh
  waited 111595 ms for the limiter and reported the cache current at 21:02:46.
- Search calls on the owner root returned substantial frames at 21:03–21:04.
- The head session logged disconnected search loads and a 60-second reload
  cooldown at 21:17:10 and 21:18:34.
- The two owner-root search calls at 21:19:23 and 21:19:26 returned 828-byte
  frames, consistent with the reported refusals.
- Borrowed roots repeatedly logged `budget_stopped`. Their configure loader
  was already a background worker but used the one-second interactive parse
  deadline, discarded partial work, and disconnected its receiver.

There is no owner-root eviction event adjacent to the refusals in the supplied
excerpt. Earlier successful builds alone do not prove a generation remained
resident: non-equivalent configure cleared both indexes, even when a change
only affected another lane. Semantic replacement progress also overwrote Ready
with Building. Both are concrete serving-generation hazards; neither can be
claimed as the proven historical cause without the missing lane observations.

## Restart/bind evidence (daemon 65964)

The owner bound at 22:14:52; its watcher started at 22:15:01. At 22:15:40
its semantic catch-up queued behind the cold-build cap and did not acquire a
slot until 22:17:46 (126803 ms). The bind catch-up then embedded only one chunk
from two files at 22:17:47. That is 175 seconds from bind to catch-up work.
The excerpt does not include an owner search-ready event or a semantic Ready
publication timestamp, so exact first-serving latency cannot be reconstructed.

The owner search loader formerly acquired a cold-build permit before reading
any disk cache. It now reopens and verifies compatible same-HEAD caches without
that permit; absent/incompatible generations still acquire one before rebuild.
A saturated-limiter regression test exercises the actual configure loader with
a persisted artifact. Semantic catch-up still uses the existing limiter; this
change does not change embedding scheduling or duplicate semantic materialization.

## Loading and retention

Borrowed configure loads now use a lifecycle-cancellable background budget
rather than an interactive parse deadline. External-path loads publish a shared
flight before starting; the interactive caller stops waiting while that same
parse continues. Later calls reuse its Arc, not a new per-call index copy.
The previous external generation remains available during replacement.

A process-wide limit admits two borrowed search parses at once. Queuing plus
parsing is bounded by five minutes, with cancellation checkpoints in the parser.
The background cap is one million records, distinct from the interactive
100000-record cap. This matters because `budget_stopped` also names record-cap
refusals, not just elapsed-time stops. Coverage checks and read-only artifact
policy are unchanged. Cache eviction/unbind drops the external flight's strong
reference; configure loads use existing lifecycle cancellation.

The external cache retains at most four entries per AppContext, shared between
search and semantic artifacts. Each search entry retains at most one serving
index plus one replacement being built. At most two replacement parses run
process-wide. Bound root contexts can each retain their own search overlay;
there is no global four-copy cap. These are not fixed-byte memory limits:
path strings, lookup tables and overlays vary by corpus, and shared mapped
postings must not be counted as independent full copies. Use SearchIndex's
estimated_memory() for a particular loaded generation. This change does not
alter semantic materialization or establish a new global resident-byte cap.

## Proposal only: no-index fallback

A disclosed bounded keyword scan when both lanes are unavailable could answer
simple identifiers immediately under load, avoiding an extra grep tool call.
It would not provide semantic retrieval, indexed ranking, or complete coverage;
limits would need to be explicit in text and structured fields. Silently
substituting it would undermine the ranked-search contract and could look like
an authoritative zero-result answer. Prefer an operator-approved opt-in mode
(or an explicitly named degraded response contract), with scanned-file/time
limits and `complete: false`. No new no-lane fallback is implemented here.

## Verification limits

Search, semantic, configure and borrowed-artifact unit suites passed, as did
`cargo check -p agent-file-tools --lib`. Disabling the semantic progress guard
made only `semantic_replacement_progress_keeps_previous_generation_ready` fail;
the guard was restored and the test passed again.

The search-quality run on the worktree debug binary passed all 16 exact-recall
fixtures and produced 26 concept rows (score SHA-256
`34207927f58119fbc8959892a0609fba8a153a8eb81b5348fe7e3cc2ea1ccf65`). The full
command timed out after 30 minutes during paged real-query replay on this loaded
machine. No byte-identical ranking comparison is claimed. Before integration,
finish the paged real-query search-quality gate on base and candidate binaries
and compare their ranked rows; the partial recall results do not replace it.
