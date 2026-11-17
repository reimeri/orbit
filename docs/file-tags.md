# Workspace file tags

Type `@` at the start of text or after whitespace in the composer to search the current workspace. The
picker supports Up/Down, Tab/Enter, mouse selection and Escape. Enter selects a
suggestion while the picker is open; it does not also send the message. Shift+Enter
still inserts a newline. An unhandled Escape retains the app's stop/dismiss behavior.
Long labels are ellipsized only on screen; hover shows the full tag. Up to 24 results
are retained and at most eight suggestion rows are mounted.

Tags are ordinary editable text, not rich-text chips. They persist with drafts,
copy/paste normally, and can be edited in their middle. Examples:

```text
Compare @src/main.rs with @"docs/design notes.md"
A literal \@not-a-tag and an email@example.com are not attachments.
```

Quoted names support `\"`, `\\`, `\n`, `\r`, `\t`, and `\xHH`. Unicode can be entered
directly. Byte escapes preserve non-UTF-8 Unix filenames without replacing bytes.
Autocomplete writes the appropriate representation. A standalone `@` is not an
attachment. Malformed quoted tags fail submission rather than attaching a prefix
or interpreting embedded `@` characters as different tags.

## What gets attached

**Tags capture file contents at send time, not when selected and not when replayed.**
This applies to first messages, ordinary messages and steering. The authored text
is unchanged in the transcript; a small manifest records each captured file's name
and exact byte size, with thumbnails for supported images.

- Paths are workspace-relative. Absolute paths and `..` components are rejected.
- Linux `openat2(BENEATH | NO_MAGICLINKS)` enforces containment while opening, not
  just during an earlier pathname check. Relative symlinks resolving inside the workspace can be used manually;
  absolute and escaping symlinks are rejected. Only regular files are accepted, with nonblocking
  opening so FIFOs cannot hang submission.
- Repeated references to the same device/inode attach once, in first-mention order.
- The current shared policy in `file_tags.rs` inlines at most **64 KiB per text file**
  and **256 KiB of file text per message**. These are inline budgets, not read limits.
- Larger files, non-UTF-8 data, and binary files remain complete. Private `/tmp`
  snapshots are identified to the model by absolute path and exact byte size.
  Accepted attachments also retain durable copies under the `pico-evo` store's
  `files/` directory; OS temporary-file cleanup does not destroy replay data.
  Missing temporary copies are recreated from those durable bytes at projection.
- PNG/JPEG/WebP/GIF data uses the existing image pipeline and its documented
  decode/size limits. Other binary data remains a file reference, not an unsupported
  audio input. Image-generation billing is not involved.
- File size/mtime/ctime are checked across capture. A concurrently modified file,
  disappeared file, bad tag, permission error, or failed preservation blocks
  acceptance with a recoverable error and keeps the draft. No 32-file cutoff,
  silent omission, lossy text decoding, or incomplete-success result is used.

Capture streams through a bounded buffer outside conversation/subagent locks.
The normal serialized send worker preserves draft/send ordering; background
filename discovery/search never occupies that worker. Captures are persisted
before acceptance clears the draft. Retrying an already accepted submission ID,
including a first-send retry, reuses the accepted message rather than rereading
changed or deleted originals. Title generation uses authored text, not expanded
file contents. Pending input, durable user entries and reconstructed history all
retain attachment metadata. Provider-only projection materializes temporary data
and strips internal `pico_file` records from wire input.

## Index and lifecycle

`pico-core/src/file_index.rs` owns the UI-independent service;
`pico-app/src/file_completion.rs` owns session/token state and presentation.

- Two services at most are retained in an app-local LRU, shared between sessions
  with the same workspace root. Selecting a workspace starts discovery early.
- Discovery/watch and search have independent workers. Latest-only query state
  and a coalescing result channel bound pending work. Closing an evicted service
  cancels without joining on the UI thread.
- A snapshot contains one byte arena, compact entry offsets, and full-path and
  basename sort orders. Paths are complete; there are no V1-style fixed query/path
  buffers. ASCII-insensitive matching preserves all original path bytes.
- Ranking is exact basename, basename prefix, full-path prefix, then substring,
  with deterministic path ties. Binary lookup handles prefixes. Prefix results
  are published **before** substring backfill, so a single good match need not
  wait for a million-file miss scan. No fuzzy/trigram database is needed.
- Discovery, cache loading, sorting and search check cancellation. Sorting uses
  bounded chunks and a reusable merge scratch array rather than uncancellable
  whole-corpus sorts. Substring/no-match search remains linear in path data.
- Query IDs, index generations and path identity reject stale replies and clicks.
  Refresh reruns the current query rather than pinning the old generation until
  the token is left. Selection follows the path, not its row number. Escape and
  acceptance cancel work and dismiss only that token.
- Discovery uses Git tracked + non-ignored untracked files, with NUL-delimited
  output. Non-Git/unavailable-Git discovery uses Rust's ignore-aware walker,
  including hidden files and `.gitignore`, `.ignore`, and `.rgignore`. There is no
  unfiltered whole-workspace fallback and no ripgrep runtime dependency.
- Directory symlinks are not recursively followed. `.git`, `.pico`, `.pico-evo`,
  the configured `pico-evo` application state,
  and the service's own cache are excluded from normal discovery.
- A directory-qualified query offers **Include ignored directory**. This explicit
  opt-in uses a separately cached, descriptor-pinned directory traversal. It does
  not change the main corpus's ignore policy or allow traversal outside the root.
  Manually entered workspace paths can also refer to ignored files.
- Path-only, versioned, private caches live under `$XDG_CACHE_HOME/pico-evo/files`
  (normally `~/.cache/pico-evo/files`). Cache identity includes root bytes and
  device/inode. Cached results are labelled provisional while live refresh runs.
  Invalid caches are rebuilt; cache write failure does not disable live search.
- Nonrecursive watches cover indexed parent directories, discovered empty
  directories, the active explicit ignored scope, Git/worktree metadata and
  conventional ignore-policy locations. Changing or dismissing the scope updates
  only its watch extension, without rebuilding the main filename index or rewriting
  its cache. Watches shared with normal discovery remain installed. Content writes
  do not reindex filenames. Event bursts debounce for 250 ms; 30-second
  reconciliation also bounds continuous-event
  deferral and recovers missed events/global configuration changes.
- Watches are reinstalled during reconciliation, including paths replaced by a
  different inode. Failed registrations are retried. Watch unavailability/budget
  exhaustion degrades visibly to periodic reconciliation. Failed discovery keeps
  the previous good snapshot and offers automatic retry plus Refresh.
- Git runs in its own process group. A supervisor handles cancellation, inherited
  stdout pipes, closed stdout followed by a hanging process, and a 120-second
  discovery timeout. No process mutex is held across a blocking wait.

The snapshot construction budget is 512 MiB (the same construction ceiling as V1), accounting for arena/entry capacities,
sort indexes and merge scratch. Exceeding it rejects the replacement, not silently
truncates the searchable corpus; narrow the workspace or ignore generated trees.
Watch bookkeeping has separate 8,192-path/8 MiB bounds. These are **not process RSS
limits**: current/replacement/search-pinned/scoped snapshots, cache serialization,
enumerator buffers, watcher internals and both LRU services can coexist. Retention
and worker counts are bounded, but peak RSS needs workload-specific measurement.

## Differences from V1

V2 keeps V1's asynchronous discovery/search split, compact path corpus, prefix
indexes, bounded results, cache-first startup, debounce and reconciliation.
Differences are deliberate:

- Built-in Rust service instead of plugin hooks; event-driven UI instead of polling.
- Workspace-only, kernel-enforced attachment opening instead of arbitrary absolute
  paths/escaping symlinks.
- Lossless long/non-UTF-8 names instead of silently unselectable long paths.
- Ignore-aware native fallback; ignored directory discovery is a visible opt-in.
- Immediately usable prefix batches, current-generation refresh, and stable clicked
  path identity rather than stale token snapshots or row-index acceptance.
- Empty-directory/scoped watches, bounded watch bookkeeping and root-replacement
  recovery, with periodic fallback when watches cannot cover the tree.
- Aggregate inline budgeting, complete binary/overflow preservation, immutable
  replay data, and explicit failed-send retention instead of omitted attachments.
- No V1 configuration, cache, session or plugin compatibility layer.

## Verification and benchmarks

Normal `just verify` includes parser/path-byte round trips; real service refresh,
empty-directory watching, root replacement and failure recovery; cache corruption;
scoped descriptor containment; cancellation and Git subprocess shutdown;
attachment containment/FIFO rejection/deduplication/aggregate overflow; persistent
replay/idempotent first-send retries; stale-result/click rejection; composer
completion/failed-send handling; and real-widget Escape/modal routing. Native
smoke includes the file-tag picker as well as the existing large composer scenes.

Run all tools through `nix develop`. Performance checks are opt-in, not
machine-dependent CI timing assertions:

```sh
# Path-only build/search comparison: 10k, 100k and 1M filenames; three samples.
nix develop --command cargo test --locked --release -p pico-core \
  file_index::tests::benchmark -- --ignored --nocapture --test-threads=1

# End-to-end worker/cache startup on a generated 10k-file workspace.
nix develop --command cargo test --locked --release -p pico-core \
  file_index::tests::workspace_benchmark -- --ignored --nocapture --test-threads=1
# Or set PICO_FILE_BENCH_WORKSPACE to an existing project (read-only discovery).
```

### Local measurements — 2026-11-17

AMD Ryzen 7 5800X3D, pinned Nix tooling. V1 `-O3`, V2 Cargo release; no concurrent
benchmarks/builds. Both use `d{i % 137}/file{i:07}.rs`, identical queries and the
production result limit. The table uses medians of three runs; each query sample
is itself a median of seven repetitions. The V2 table uses `v2-optimized.log`; `v2.log` preserves the initial implementation
measurements. Complete logs are in
[`benchmarks/file-tags-2026-11-17/`](benchmarks/file-tags-2026-11-17/).

| One million paths | V1 | V2 |
|---|---:|---:|
| In-memory construction/sorting | 556 ms | 590 ms |
| Basename prefix with only one match: first usable batch | 30.59 ms | 0.004 ms |
| Same query: complete substring backfill | 30.59 ms | 27.15 ms |
| Path prefix filling the result batch | 0.003 ms | 0.003 ms |
| Substring query filling the result batch | 1.81 ms | 1.29 ms |
| No-match query | 26.88 ms | 7.91 ms |
| Retained path/index payload, not RSS | 35.20 MB | 39.20 MB |

V2 spends four additional bytes per path on precomputed entry metadata in this
fixture. First-batch figures above measure **search execution**, not UI frames or
worker scheduling. V1 waits for backfill before publishing its results; V2 does not.

For the real service on 10,000 generated files, median time to the first usable
result was **89.3 ms with no application cache**, versus **5.31 ms from the cache**.
These include worker scheduling, and the cold case includes discovery/watch setup
and cache publication. The fixture was just created: this is not an OS page-cache
flush or cold-storage benchmark. Background validation continues after cached
results appear.

The measurements establish comparable construction/search costs and useful warm
startup, not universal latency or memory guarantees. Filesystem discovery over a
million actual files, peak RSS under simultaneous rebuilds, network filesystems,
all Unicode distributions, and end-to-end frame latency remain separate profiling
workloads. A separate opt-in `deep_paths_benchmark` exercises a million substantially longer
paths, including Unicode and deep directories, so the capacity check is not limited
to the short-name comparison corpus. Long paths, ignored trees, generations,
cancellation, and editor behavior
are protected by deterministic correctness tests rather than this timing fixture.

The deep/Unicode million-path fixture retains 148.3 MB including 128.3 MB of path
bytes, rather than relying on short names to fit the index budget. One optimized
sample built in 3.30 seconds; its basename-prefix batch appeared in 0.016 ms,
complete backfill took 80.1 ms, and a no-match scan took 75.3 ms. Before adding
SIMD first-byte candidate filtering (`memchr`), the same long-path searches took
about 258 ms and 256 ms respectively. These are single-sample diagnostic figures,
not cross-machine guarantees. They illustrate why backfill stays cancellable and
off the UI thread even after optimization.
