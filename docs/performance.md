# Long-session and large-library performance

Target workloads: **10,000 saved sessions, 20,000 transcript entries in a selected
session, and eight active conversations**. These are evaluation targets, not hard
limits. Execution limits remain per conversation; no global admission cap was added.

## Architecture

- `sessions.sqlite3` is the session store. Metadata, ordered transcript records,
  and ordered provider context commit in one SQLite transaction (WAL mode,
  `synchronous=FULL`). Persistent tree diffs update only changed records. A draft
  update does not serialize or rewrite historical entries or provider input.
- A serialized UI I/O worker performs loading, editing, deletion and catalog/draft
  saving. Conversation persistence has its own ordering lock; it snapshots state
  before writing and does not hold the conversation state lock across disk I/O.
  Agent async tasks offload saves to blocking workers. Message acceptance commits
  before it is published; tool-start boundaries commit before commands launch.
- Startup loads lightweight session summaries, not transcript bodies. Selected
  and executing conversations load bodies on demand. Idle unselected bodies are
  released; runtime stubs retain unread cell/process results and JS store values
  so changing sessions does not invalidate runtime IDs. Attention status is
  persistent metadata, independent of loaded conversation objects.
- Indexed, structurally shared histories make snapshots cheap and edits by ID
  logarithmic/hashed rather than full-transcript searches. Tree differences skip
  unchanged shared subtrees. Provider input is shared, not deeply copied into each
  UI snapshot.
- Chat and sidebar construct rows only around their viewports. A Fenwick height
  index supports logarithmic offset lookup and measurement updates. Transcript
  append is incremental; full visibility rebuilding is reserved for group changes.
  Measured sidebar heights and the visible anchor follow stable session identity.
  Epoch-qualified measurements reject stale events after a switch/reflow.
- Markdown is cached only around the viewport; visible streaming Markdown uses
  iced's incremental `Content::push_str`. Collapsed reasoning is not parsed.
  Tool output retains its fixed-row shaping window. Transcript entries and tool
  rows use true key-to-widget-tree reconciliation: overlapping rows keep their
  shaped paragraphs even when a same-sized viewport advances. Transcript identity
  is session/entry ID, separate from measurement epochs; tool rows use source byte
  starts and still invalidate changed text/wrapping. Unmounted trees are dropped.
  The sidebar shares this implementation with optional widget-local animation.
  Selectable Markdown, notices and tool rows use one paragraph for drawing,
  selection and link hits, including inline-code backgrounds and decorations.
  Copy messages carry entry IDs, not full message strings embedded in every tree.
- Fallback compaction persists its plaintext summary and transcript checkpoint;
  later fallbacks summarize only subsequent records. It builds bounded chunks and
  does not copy the original provider input into each summary request. Native
  compaction remains the preferred path. SSE decoding scans incoming bytes once
  and bounds the combined pending line/event storage.
- Shell output uses a bounded ring/deque instead of shifting its 1 MiB tail.
  Non-PTY pipes/waiters and PTY readiness use the shared Tokio executor; a PTY
  child still has a blocking wait thread. QuickJS cells and nested host calls
  retain dedicated worker threads, bounded by the existing per-conversation limits.
  Waiting cells wake on completion, cancellation or their next timer deadline,
  not a 2 ms polling cycle. Output/store size accounting is incremental.
- Git refresh is immediate on selection, normally every 5 seconds for an active
  selected turn and 60 seconds when idle, with cost-based backoff up to 5 minutes.
  Requests have a 15-second timeout and do not block the UI.

### Durability and recovery

New-session drafts occupy a singleton SQLite row, not a session row. Their message
is shared across project choices and loaded on restart. The first send commits the
session and its user message while deleting the shared draft in the same transaction.
Existing-session composer drafts remain session-local.

Drafts debounce on a one-second timer. Switching sessions flushes the old draft
before loading the new session. Composer input/autosave is temporarily suspended
while a selection/send transition is pending, while the window continues updating.
Shutdown stops accepting edits, runs after earlier UI jobs, cancels execution,
waits for turns to finish, and saves drafts/catalog/session state and credentials.
A failure leaves the window open with an actionable message.

Failed message acceptance is not inserted into the in-memory queue. Retrying a
successfully accepted submission uses its identity and does not queue or start it
again once it has been delivered. Failed ordinary edits stay in memory for retry.
Cancellation frees inaccessible command slots; actual cell-thread lifetime is
tracked independently of retrievable cell IDs so eviction cannot race callbacks.

**Format change:** old `sessions/*.json` files are neither migrated nor loaded.
They remain untouched. Only the new SQLite library is displayed. Settings and
credentials retain their existing files under `pico-evo`. Close Pico before making
a file-level backup; if copying a live database, its WAL is part of the state—use
SQLite's backup facility rather than copying just the main database file.

## Reproducing the measurements

```sh
nix develop --command cargo run --locked --release -p pico-core --example performance
nix develop --command just verify
nix develop --command just smoke-gpu   # when a Wayland desktop is available
```

The example creates isolated temporary state, uses no credentials/network, and
preserves the normal build cache. Its transcript has 20,000 approximately 1 KiB
entries plus 2,000 provider-context items. Its library fixture has 10,000 summary
rows and one large body: the summary query does not read any bodies. Baselines
reproduce the old deep-copy/full-pretty-JSON-write algorithms on the same fixture;
they are not an end-to-end comparison between separately built application versions.

One local release run (milliseconds per operation):

| Operation | Time |
| --- | ---: |
| Deep-copy transcript and provider context baseline | 13.72 ms |
| Shared full-session snapshot | 0.000077 ms |
| Full JSON durable rewrite baseline | 81.92 ms |
| Incremental durable draft update | 1.89 ms |
| Incremental durable one-entry update | 3.43 ms |
| Load 10,000 sidebar summaries | 23.83 ms |
| Load selected 20,000-entry session | 44.13 ms |
| Height-index viewport lookup | 0.000019 ms |
| Height-index append | 0.000047 ms |

At entry 15,000, the viewport/overscan range held **16 of 20,000 rows**. A process
waiting for a JS timer accrued zero `/proc/self/stat` CPU ticks during a one-second
sample; that is limited-resolution evidence, not a claim of literally zero work.

Storage timings are particularly variable: an earlier run measured approximately
38–50 ms for incremental saves. `fsync` latency, filesystem/storage activity and
cache state remain relevant. Do not treat the table as a cross-machine guarantee
or infer application frame rates from these microbenchmarks.

## Text-layout cache reuse

A local isolated measurement used a real ~33 KiB tool result, a 700px-wide tool
viewport and 22 mounted rows. Each step advanced the viewport by one row, rebuilt
iced's UI and retained its widget cache (100 updates, the first stationary).

| CPU-side layout per update | Before | After |
|---|---:|---:|
| Debug: one-row advance | 88.06 ms | 2.15 ms |
| Release: one-row advance | 7.84 ms | 0.19 ms |
| Release: unchanged visible rows | 0.008 ms | 0.008 ms |

Previously, positional widget reuse changed the text in almost every slot;
painting and selection each shaped an independent paragraph. The measured phase
constructed 2,154 selection paragraphs plus the independent painting paragraphs.
Afterwards it constructed **99 total paragraphs**, one per newly entering row.
The same identity fix applies to sliding transcript windows. Supplying keys to
iced 0.14's `keyed_column` alone is insufficient: its splice heuristic still
diffs equal-length lists positionally. `keyed_column.rs` remaps the actual trees.

These are local microbenchmarks, not end-to-end frame times or an application CPU
usage guarantee. They use the software renderer's shared CPU text-layout path,
simulated offsets, no draw/present, and separately instrumented test builds.
First-use font initialization also affects expansion; the change does not remove
that cost. Normal full-output normalization/reflow remains linear in text size.

Deterministic regressions count new paragraphs for forward/reverse tool and chat
scrolling, changed text, width reflow, measurement epochs and session/entry identity.
They also check eviction rather than an unbounded offscreen paragraph cache.
Software and opt-in GPU pixel comparisons against iced Rich cover span styles,
Unicode/bidi, wrapping, fractional scaling and scroll clipping. Run the GPU
comparison explicitly on a machine with a GPU adapter:

```sh
nix develop --command cargo test --locked -p pico-evo shared_paragraph_matches_rich_text_pixels_gpu -- --ignored
```

## Activity-group label fitting

Opening a 37-tool group exposed repeated shaping in `ellipsized_text.rs`: each
new label measured its complete text, then shaped a binary search of candidate
prefixes, then measured the winner. This was not eager tool-output rendering.

Fitting now uses the full paragraph's glyph-cluster advances in **logical byte
order** to choose a grapheme-safe prefix. It measures the ellipsis and verifies
the final candidate, backing off only if contextual reshaping makes it too wide.
A fitting label needs one measurement; ordinary truncation needs three instead of
an unconditional series of binary-search probes. Ligatures/complex scripts may
produce a slightly shorter cutoff. The final paragraph must fit, and full source
text remains available to widget operations and tool details. Grouping, overscan,
tool-output caching and compiler profiles are unchanged.

A local **debug** before/after microbenchmark used a read-only snapshot of a real
293-entry session with eight activity groups. The target group contained 37 tools
and 33 reasoning entries. Both versions used the same snapshot, dependency
artifacts, debug flags and software-renderer UI construction/layout path. Each
width was tested in three fresh processes per version, with two group openings
per process; the table reports medians of six openings. Closing unmounts rows, so
these include reopening, not just a warm unchanged-layout measurement.

| CPU-side operation | Before | After |
|---|---:|---:|
| Expansion layout, 1280px window | 181.39 ms | 65.46 ms |
| Expansion layout, 1024px window | 157.80 ms | 63.36 ms |
| Label fitting within layout, 1280px | 172.82 ms | 57.16 ms |
| Label fitting within layout, 1024px | 149.48 ms | 55.12 ms |
| Text measurements per opening, 1280px | 186 | 61 |
| Text measurements per opening, 1024px | 184 | 61 |

Expansion layout was **60–64% faster (2.5–2.8×)**. The harness mounted 49 transcript
rows, including 23 tool rows, and **zero expanded tool-output caches**. Expansion
handling/index preparation remained about 0.14 ms, widget construction about
0.75–0.80 ms, and unchanged cached layout about 0.85 ms. Full-label and final-label
shaping still cost time; this does not establish a sub-frame debug expansion.

These are CPU microbenchmarks, not click-to-screen latency or release-build
claims. They exclude draw/present and subsequent row-size/scroll events. The
snapshot and instrumented harness stayed in private temporary files; no saved
session was modified or added to the repository. Regression tests use real fonts
to cover fit boundaries, combining marks, joined emoji, mixed RTL/LTR, retained
widget caches and the ordinary measurement count; a deterministic width-change
fixture exercises the contextual-overflow correction path.

## Verification and remaining measurement limits

Regression coverage includes incremental SQL writes and atomic rollback, failed
submission retry/restart, interrupted-session reload positions, runtime-preserving
body eviction, cancelled native callbacks, command-slot reuse, exact tail overflow,
Unicode output caps, timer fairness, compaction checkpoint reuse, overlapping UI
transitions/autosaves/shutdown, stale measurements, and bounded 20,000-entry caches.

Software and GPU smoke scenarios exercise a real Wayland window: large-output
scrolling, long transcript middle/end/reflow, a 10,000-session sidebar, and renewed
measurement identity. They do not replace exhaustive interaction testing.

These measurements do not include live model latency, eight simultaneous real
model streams, or a full frame-time/allocation profile. Loading one huge selected
body still takes time off the UI thread; formatting one enormous indivisible
Markdown block may still be expensive, since virtualization is at message level.
Compaction still requires sequential model calls for its new chunks. Resource use
can grow with deliberately retained runtime results/JS store data across many
conversations because the requested policy has no global cap.
