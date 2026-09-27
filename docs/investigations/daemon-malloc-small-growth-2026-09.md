# AFT daemon footprint: ~9 GB of dirty "Malloc Small" (September 2026)

Research only. No product code changed. The live daemon (`ck-aft`, pid 93114)
was observed only through `footprint`, `ps`, its log, the passive
`memory.census` management op (`ck-aft profile --memory --json`), and the
ordinary `status` tool on an already-bound root. Nothing suspended or attached
to it. Heap tools were used only on isolated processes started for this work.

## Answer

1. **The 8.2 GB is live, not freed-but-retained.** libmalloc's own counters in
   the live daemon read `bytes_in_use` = **8,889,150,528** with
   `phys_footprint` = 9,854,895,504 (07:39Z). Live bytes are about equal to the
   footprint, so fragmentation cannot be more than about a gigabyte of it. The
   "allocator slack" the periodic relief line reports (13.9-14.2 GB) is address
   space the allocator has already handed back; it is not in the footprint.
2. **Most of the live memory is invisible to AFT's own accounting.** The census
   attributed only **3.1-3.3 GB** to roots; **5.4-5.6 GB is live and
   unattributed**.
3. **One confirmed cause of unattributed live memory:** a semantic index loaded
   from disk after a bind is never installed when the route closes before the
   next completion drain. It stays alive until the idle-TTL reaper drops the
   root (30 min by default, never for roots whose routes stay bound), the census
   does not count it, and the reaper's "freed" line reports 0 MB of semantic
   data for it. Reproduced in an isolated daemon: six short sessions left
   **919.7 MB** of semantic indexes live while the census grew by **56 MB**
   (`crates/aft/tests/daemon_malloc_small_growth_repro.rs`,
   `parked_semantic_loads_are_unattributed`, `#[ignore]`, fails today).
4. **Not the driver:** cold search-index rebuilds after watcher overflow and
   repeated semantic loads both release everything once their result is
   dropped (two passing controls in the same file). Search rebuilds line up
   with the RSS spikes in the log, but they are transient.
5. **The relief loop does nothing on this macOS.** On macOS 27.0 (26A428) the
   default allocator returns whole free pages by itself as they empty ("Malloc
   Small" reclaimable), and `malloc_zone_pressure_relief` returned 0 in every
   case measured, including with 1 GB of free pages available. The
   `phys_footprint_drop_bytes=Some(0)` lines are expected and say nothing about
   whether memory is free.

What remains unexplained: this investigation proves the mechanism and its
invisibility, not that it accounts for all 5.4 GB in the live process. The
remainder cannot be split further without attaching to pid 93114. Suspects are
ranked below.

## Live daemon evidence (pid 93114, started 2026-09-26 20:53Z)

### footprint(1)

| time (Z) | footprint | Malloc Small dirty | regions | reclaimable | Malloc Large |
| --- | ---: | ---: | ---: | ---: | ---: |
| 07:21 | 8,929 MB | 8,259 MB | 5,266 | 0 B | 546 MB |
| 07:35 | 9,394 MB | 8,686 MB | 5,266 | 0 B | 583 MB |
| 07:39 | — | 8,691 MB | 5,266 | 0 B | 583 MB |

`phys_footprint_peak` was **15 GB**. RSS was 0.5-1.5 GB throughout; the rest is
compressed or swapped (system swap was 11.8 of 12 GB used). Reclaimable 0 B
means no freed page is waiting to be reclaimed; the isolated tests below show
that on this allocator a freed whole page shows up there until the kernel takes
it.

### Allocator counters and census

`status` (tool call on an already-bound worktree root, session
`mason-malloc-probe`, via `subc-probe`):

| time (Z) | bytes_in_use | size_allocated | footprint | attributed | unattributed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 07:39 | 8,889 MB | 22,833 MB | 9,855 MB | 3,262 MB | 6,593 MB |
| 07:41 | 8,479 MB | 21,775 MB | 9,413 MB | 3,104 MB | — |
| 07:47 | 8,662 MB | 21,787 MB | 8,911 MB | 3,103 MB | — |
| 07:52 | 8,666 MB | 21,787 MB | 8,990 MB | 3,093 MB | — |
| 07:59 | 8,656 MB | 21,787 MB | 8,999 MB | 3,086 MB | — |

The `memory.census` op (07:27Z) listed 47 roots, footprint 9,566 MB,
attributed 3,235 MB (1,543 MB of it the umbrella root's semantic index),
unattributed 6,331 MB. At 08:11Z most roots still had bound routes whose last
request was 5-11 hours old, so the idle-TTL reaper never considers them.
The process had 477 threads.

### Log correlation (`aft-93114.log`)

- `allocator slack relief` every 5 min: `phys_footprint_drop_bytes=Some(0)`
  every time. Address-space slack rose from 1.3 GB (20:56) to 14.2 GB (23:00)
  and stayed flat to 07:21.
- RSS at the relief sample spiked to 9-11 GB at 21:21, 21:51, 22:36, 23:01 and
  01:56. Every spike window contains cold streaming builds of the umbrella
  root's search index (29,787 files, 177,849 trigrams, 12-19 s each; 10 of
  them in 21:50-21:59, 4 each in 22:30-22:39 and 23:00-23:09). They are
  triggered by `watcher overflow: reason=user_dropped` on the umbrella root.
  RSS returned to 0.5-1 GB after each, so these are transient.
- Tier-2 on the umbrella root is cheap (`reason=no_callgraph`, ~1 s); the
  heaviest tier-2 snapshot was prefrontal's (402k edges).
- 47 `loaded semantic index from disk` events loaded 869,199 entries in total;
  28 of them (579,185 entries) in the first 40 minutes. The same 7,142-entry
  index was loaded 9 times, and one worktree loaded the same 7,278-entry index
  3 times within a second (07:23:27-28Z). The isolated daemon measured about
  5.7 KB of heap per loaded entry (vector plus strings), so 869k entries is
  about 4.9 GB allocated over the run.
- 70 `evicted idle root` lines; 24 of them report `freed ~0.0 MB`.

## Allocator behaviour on this macOS (isolated C programs)

All sizes from 48 B to 20 KB land in "Malloc Small" (1 GB of 64-byte objects
shows as 1,024 MB Malloc Small, 1 region of Malloc Tiny). Each region is 4 MB;
`size_allocated` equals regions × 4 MB and never drops after frees.

| workload | live after | Malloc Small dirty | reclaimable | relief returned |
| --- | ---: | ---: | ---: | ---: |
| 1 GB of 48 B / 512 B / 4 KB objects, all freed | ~0 | 17-19 MB | 1,007 MB | 0 |
| 1 GB of 20 KB objects, all freed | 0 | 226 MB | 893 MB | 0 |
| 1 GB of 64 B, keep 1 in 16 or 1 in 256 | 64 / 4 MB | 1,024 MB | 0 | 0 |
| 1 GB of 64 B, keep 1 in 4,096 | 0.25 MB | 529 MB | 495 MB | 0 |
| 1 GB of 4 KB, keep 1 in 8 | 128 MB | 531 MB | 495 MB | 0 |
| 15.6 GB mixed 16 B-30 KB, keep 1 in 7 | 2,350 MB | 13 GB | 1,557 MB | — |

So: freed whole pages are returned without any relief call; sparse survivors
pin whole chunks (fragmentation is severe when it happens), and
`malloc_zone_pressure_relief` never returns anything. The live daemon does not
look like the fragmented rows: its live bytes are close to its dirty bytes.

`vmmap -summary` is **not** safe on the live daemon: on a 2.7 GB test heap it
stopped the target for 0.97 s (a 1 ms heartbeat thread in the target recorded
the gap). It was not run on pid 93114.

## Isolated daemon reproduction

A hermetic `ck-subc` (same binary as production, `SUBC_PORT=0`, every XDG
directory, `HOME` and `TMPDIR` under `/tmp/aftrig`) with a stub
OpenAI-compatible embedding server, first running the production `ck-aft`
0.57.2 and then a debug build of this commit with `MallocStackLogging=1`.
Roots were a 21,848-file copy of several CortexKit repositories plus twelve
worktree-style copies. Driven with `subc-probe` (one route per call, closed
when the probe exits).

- **Mixed workload** (search, grep, outline, inspect per root, 4,000-file churn
  bursts in the umbrella copy): footprint swung between 0.2 and 3.2 GB while
  the census attributed at most 168 MB. `heap` found 1.9 GB live in 4.0 M
  allocations. The largest live allocation site (debug build, symbolicated)
  was `SemanticIndex::read_from_disk` called from
  `commands::configure::schedule_artifact_loads`: 81,936 allocations, 400 MB,
  exactly three 27,312-entry indexes, none of them counted by the census.
- **Eviction check** (idle TTL set to its 5-minute minimum): nine short
  sessions loaded seven indexes. 60 s later 314 MB of `read_from_disk`
  allocations were live, footprint was 496 MB and the census showed
  `semantic 0 B` for every one of those roots. After the reaper ran, the
  reaper logged `semantic 0.0 MB` for each root and the `read_from_disk`
  allocations were gone. The memory is held until eviction, not leaked
  forever.
- **Timing matters.** When the session closed before the load finished (a 2 s
  search), the loader saw an unbound root and dropped the index (`failed to
  build semantic index: semantic post-bind refresh cancelled because root is
  unbound`): nothing was retained. When the session outlived the load (a 15 s
  command), all six indexes stayed live and unattributed. Agent sessions that
  run commands or wait on a model routinely last longer than a load.

The committed reproduction automates the last case and fails today:

```text
baseline                           footprint=16.4MB  attributed=0.1MB  unattributed=16.3MB
after one short session per root   footprint=677.2MB attributed=56.2MB unattributed=621.0MB
live bytes allocated under SemanticIndex::read_from_disk: 919.7 MB
```

## Growth drivers checked

| event in the log | effect in isolation | verdict |
| --- | --- | --- |
| search index refresh after watcher overflow | 8 cold rebuilds of 4,000 files: live 2.5 MB, dirty Malloc Small flat at 9-14 MB; total footprint 104-113 MB, of which 96 MB is freed-but-dirty Malloc Large (one run climbed to 398 MB) | transient for Malloc Small; explains the RSS spikes, not the retention. Freed large buffers (the spill block is up to 128 MB) can stay dirty in Malloc Large, a separate, bounded effect consistent with the live daemon's 0.5 GB of Malloc Large |
| semantic load from disk, previous copy dropped | live 285 MB with one copy; after drop live 0.8 MB, Malloc Small 7 MB dirty and 603 MB reclaimable | no retention; the allocator returned the pages without a relief call |
| semantic load for a root whose route then closes | 150 MB per 27k-entry index retained, unattributed | **retained until idle eviction** |
| route bind / unbind | each bind reruns configure and reloads every artifact from disk | multiplies the row above |
| tier-2 scans | umbrella tier-2 ~1 s, `no_callgraph` | not a driver here |
| semantic refresh after watcher batches | small (1-19 files per refresh) | not a driver here |

## Suspects in code, ranked

1. **Artifact loads that finish after the last drain of a closing route.**
   `commands/configure.rs:4145` `schedule_artifact_loads` spawns the loaders;
   the read-only semantic loader sends `SemanticIndexEvent::Ready(index)` into
   an unbounded channel (`configure.rs:4482-4483`, send at `4543-4549`, guarded
   only by `run_if_current`), and search does the same with `SearchIndex`
   (`configure.rs:4210-4211`). The receiver is drained only by the
   `CompletionDrains` maintenance job (`subc/mod.rs:7178-7183`).
   `quiesce_unbound_root` (`subc/mod.rs:1392-1439`) cancels queued maintenance
   and keeps the root warm by design, so a payload that arrived before the
   quiesce stays in the channel. The census reads only the installed slot
   (`context.rs:8947-8959`), so the payload is invisible, and the idle reaper's
   `freed` line comes from the same estimate. Confirmed by reproduction.
2. **Every bind reloads every artifact.** A new session on a root that already
   holds a current index still runs `schedule_artifact_loads` and reads the whole
   semantic index from disk again (log: the same index 9 times; one root 3 times
   in a second). Each reload allocates about 5.7 KB per entry. Combined with 1,
   this multiplies retained copies, and it is the source of large transient
   allocation even when nothing is retained.
3. **Roots whose routes stay bound never become evictable.** At 08:11Z most of
   the 47 roots had bound routes with no request for 5-11 hours
   (`evictable_in_ms` is null while any route is bound). Anything parked or
   duplicated on such a root lives for the life of the daemon, which is how a
   "held until eviction" payload becomes permanent growth.
4. **Private copies of shared semantic bases.** A borrowing root loads a full
   private copy when the shared base's fingerprint or artifact hash changed
   (`semantic_index.rs:7275-7288`, 4 occurrences in the log), and
   `materialize_shared_base` (`semantic_index.rs:4394-4448`) clones every base
   entry into the borrower. At 08:11Z one worktree carried 180 MB of semantic
   data while its owner root carried 0.
5. **Per-entry layout of the semantic index.** Each `EmbeddingEntry`
   (`semantic_index.rs:3520-3549`) owns six heap objects (path, name,
   qualified name, embed text, snippet, vector), all in the "Malloc Small"
   range. The estimate (`semantic_index.rs:3694-3749` and the matching
   `SemanticIndex` estimate) counts `len()`, not capacity or allocator
   rounding, so the attributed figure is itself low.
6. **Per-root threads.** 477 threads for 47 roots. In the isolated daemon each
   root's watcher, FSEvents stream, status emitter, inspect dispatch loop and
   bash watchdog showed a 1-4 MiB allocation attributed to thread creation
   (28.7 MB for 28 watcher threads, 80.5 MB for 20 executor threads). This was
   seen with `MallocStackLogging` on and was not verified without it; if real,
   it is a few hundred MB, not gigabytes.
7. **The relief loop** (`memory.rs:1448`). It cannot help on this allocator; it
   only costs two statistics walks every 5 minutes and prints a misleading
   zero.

Checked and cleared: the transient search-cache build-lock registry
(`search_index.rs:4020-4036`, weak values, pruned at 1,024 entries), the shared
semantic base registry (`semantic_index.rs:3683-3688`, weak values), and the
cold search build itself (`search_index.rs:3493-3649`, bounded by the 128 MB
SPIMI block).

## Fix options (ranked; nothing built)

1. **Handle completion payloads when a route closes.** On quiesce, either drain
   pending completion receivers into the context (the index becomes installed,
   attributed and reusable on rebind) or drop pending payloads (memory is freed;
   the next bind reloads). Draining keeps the "stay warm" intent of
   `quiesce_unbound_root`; dropping is simpler and bounds memory. Either way,
   also count pending payloads in `memory_estimates` so the census and the
   reaper's log stop reporting 0. Small, local change; fixes suspect 1 and makes
   the rest measurable.
2. **Reuse a resident, current index on rebind** instead of reloading it
   (compare artifact generation / fingerprint before `read_from_disk`). Removes
   most transient semantic allocation and the duplicate copies of suspect 2.
   Needs a correct freshness check, which is the risk.
3. **Make idle roots evictable even with bound routes.** Treat a root whose
   routes have been silent for the idle TTL as evictable for artifacts (not for
   the route), or evict least-recently-used roots when footprint passes a
   budget. Bounds suspect 3. Cost: reloads for long-idle sessions that come
   back.
4. **Stop materializing private copies of shared bases** where an overlay would
   do (suspect 4). Larger change in the borrowing design.
5. **Denser semantic storage.** One contiguous vector buffer per index (or f16,
   or memory-mapped `semantic.bin`) and an arena for the strings would cut
   allocations per entry from six to near zero, shrink the index and remove the
   fragmentation exposure the allocator tests show. Largest change.
6. **Retire the macOS relief loop** or report it honestly. Cheap; changes no
   memory, only removes noise. A different global allocator (for example
   mimalloc with its own purge) is possible but changes behaviour everywhere
   and does not address live retention.

## Reproducing

- Live observation, safe: `footprint <pid>`,
  `ck-aft profile --memory --json`, and a `status` tool call through
  `subc-probe` on a root that is already bound.
- Isolated daemon: `crates/aft/tests/daemon_malloc_small_growth_repro.rs`.
  `parked_semantic_loads_are_unattributed` needs `AFT_REPRO_SUBC_BIN`
  (a `ck-subc`) and `AFT_REPRO_SUBC_PROBE` (a `subc-probe`) and takes about 4
  minutes; it failed identically in two consecutive runs (919.7 MB live under
  `read_from_disk`, attributed +56 MB). With sessions shorter than the load
  (a 2 s search) the same harness passes, which is the negative control for
  its measurement. The two `control_*` tests run in-process in about a minute
  each and assert on dirty Malloc Small.
- Allocator behaviour: the C programs used are small (allocate N objects of
  size S, free all but every k-th, call `malloc_zone_pressure_relief`, print
  `malloc_zone_statistics` and run `footprint` on themselves); the numbers are
  in the table above.
