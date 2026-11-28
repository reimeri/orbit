# Long-session and large-library performance

Target workloads: **10,000 saved sessions, 20,000 transcript entries in a selected
session, and eight active conversations**. These are evaluation targets, not hard
limits. Execution limits remain per conversation; no global admission cap was added.

## Session storage and loading

- `sessions.sqlite3` is the session store. Metadata, ordered transcript records,
  and ordered provider context commit in one SQLite transaction (WAL mode,
  `synchronous=FULL`). Persistent tree diffs update only changed records. A draft
  update does not serialize or rewrite historical entries or provider input.
- UI storage jobs are FIFO for the resources they touch: a parent and its
  children form one family; the shared new-session draft and catalog have
  their own lanes. A switch flushes dirty drafts before opening its target,
  without reserving a clean outgoing family. Stop waits for a preceding send
  or filing in its own family, not for unrelated families. Project deletion
  and final shutdown still drain all earlier jobs. Periodic pruning has its
  own lane and never runs as part of a response save or statistics read.
  Core lifecycle operations take family-specific admission locks; cold loads
  and eviction exclude each other by session ID, even during metadata recovery.
  Conversation persistence has its own ordering lock; it snapshots state
  before writing and does not hold the conversation state lock across disk I/O.
  Agent async tasks offload saves to blocking workers. Message acceptance commits
  before it is published; tool-start boundaries commit before commands launch.
- Cold session loads use a separate read-only WAL connection. Metadata,
  transcript, provider context and fork points come from a single committed
  snapshot, even during a turn save. Large transcript/context batches decode in
  parallel and are indexed in their original order. Fork deltas stay encoded
  until a fork or the first new checkpoint needs that historical state. The
  reopened session caches decoded prefixes with structural sharing; new live
  checkpoints share the original histories. An unreadable fork delta blocks a
  dependent fork or checkpoint without discarding the transcript. Loading an
  already-resident session does not take its persistence lock. Clean switches skip unchanged session and
  shared-draft writes; draft edits, recovery and acknowledgements still commit.
  Inactive-session cleanup is queued after the destination is ready, so it
  doesn't hold up display or evict the outgoing session if the switch fails.
- Startup loads lightweight session summaries, not transcript bodies. Selected
  and executing conversations load bodies on demand. Idle unselected bodies are
  released; runtime stubs retain unread cell/process results and JS store values
  so changing sessions does not invalidate runtime IDs. Attention status is
  persistent metadata, independent of loaded conversation objects.
- Subagent startup restores the newest readable assignment per child and any
  work that needs interruption repair or notice delivery. Older failures remain
  acknowledgeable without retaining their metadata: selection captures a cutoff
  per child so a later failure stays unread. Historical waits and retried spawn
  requests use parent-scoped SQLite lookups. The transcript fetches visible
  historical assignment details on a blocking reader, not while drawing or
  waiting in a send lane. Activity summaries likewise use independent WAL reads.
  Sidebar and transcript reads use a separate in-memory assignment index, so
  filing a cold session does not block drawing while its history loads or saves.
  See the [launch comparison](benchmarks/session-and-rendering.md#historical-subagent-assignments).
- Indexed, structurally shared histories make snapshots cheap and edits by ID
  logarithmic/hashed rather than full-transcript searches. Tree differences skip
  unchanged shared subtrees. Provider input is shared, not deeply copied into each
  UI snapshot.
- Fallback compaction persists its plaintext summary and transcript checkpoint;
  later fallbacks summarize only subsequent records. It builds bounded chunks and
  does not copy the original provider input into each summary request. Native
  compaction remains the preferred path. WebSocket continuations select the new input before
  materializing saved media, and inbound event sizes are bounded.

## Durability and recovery

On the first load after a restart, interrupted commands, abandoned tools, and an
unfinished response are repaired in one transaction before the conversation is
made available. If the write fails, reopening retries from the on-disk checkpoint;
it does not replay commands or duplicate recovery notices.

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

### Diagnosing a slow send

Run `ORBIT_TRACE_SEND=1 orbit` from a terminal. Each submitted message prints
one stderr line after the storage worker returns, on success or failure. It
reports time queued on the worker, preparation (including file/skill capture),
lifecycle and persistence lock waits where applicable, the database mutex,
SQLite transaction body and commit, and publication/turn start. A SQLite writer
outside Orbit can cause a wait in the transaction body, not the Rust mutex.
Labels and elapsed times contain no draft content, paths, or session IDs.
The total starts at worker enqueue; attachment preparation, rendering, and the
model response aren't part of it. A blocked Enter hasn't submitted a message
and therefore produces no send trace.

#### Contention check

These local debug measurements use `ORBIT_TRACE_SEND=1` and temporary ext4
stores (one baseline run, medians of three runs after the change).
In each probe session A holds a controlled gate. The old implementation only
made progress after the two-second test watchdog released it; the new one
finishes session B's action **before** A is released.

| Probe | Phase measured for B | Before | After |
| --- | --- | ---: | ---: |
| A's send waiting for its persistence lock | Family/lifecycle lock wait | 2,005 ms | 0.008 ms |
| A's filing lane waiting, B sends from the UI | Storage queue wait | 2,000 ms | 0.059 ms |

Reproduce with `just test unrelated_send_and_stop -- --nocapture` and
`just test send_bypasses_another -- --nocapture` in the dev shell with the trace
enabled. The assertions are about ordering, not a wall-clock performance target.
For example, the three UI sends took 2.5–142 ms in total; the slow one spent
141 ms in SQLite commit and only 0.054 ms waiting in the queue.
SQLite still admits only one writer. Holding an external writer transaction
still caused a send to fail after 5 s in a separate probe (the trace attributed
the wait to the SQLite transaction body); an unrelated Stop call returned
while that writer was still held. Failed acceptance published no user row,
and retry after release committed one.

### Diagnosing a slow session switch

Run `ORBIT_TRACE_SESSION_SWITCH=1 orbit` from a terminal. Each sidebar switch
prints elapsed times for the storage queue, outgoing and shared drafts, load,
completion acknowledgement, model reconciliation, and applying the
selection in the UI. It prints no session content. UI update time does not
include layout, drawing or frame presentation. A long queue wait can be a
previous storage job rather than a slow destination read. For an example that
times the displayed transcript rather than just the storage worker, see the
[cold sidebar switch measurements](benchmarks/session-and-rendering.md#cold-sidebar-switch-to-a-tool-heavy-session).

The regression test `just test clean_cold_switch -- --nocapture` keeps a SQLite
writer transaction open throughout a switch between two clean sessions. Before
the change the switch failed after the 5 s busy timeout. With the writer still
held, one local debug run took 0.5 ms from the selection request to the
destination composer being ready. This is not a frame-presentation time or a
latency bound for large transcripts or switches that have unsaved work.

For state file locations, see [Directories & Configuration](../README.md#directories--configuration).

## Rendering

- Chat and sidebar construct rows only around their viewports. A Fenwick height
  index supports logarithmic offset lookup and measurement updates. Transcript
  append is incremental; full visibility rebuilding is reserved for group changes.
  On cold open, prose height estimates account for wrapping at the current lane
  width/font size and a bounded sample of hard breaks. This keeps a tall old
  prompt out of the initial bottom viewport when more recent replies fill it.
  Measured sidebar heights and the visible anchor follow stable session identity.
  Epoch-qualified measurements reject stale events after a switch/reflow.
- Wide conversations use the spare left margin for a clickable prompt rail. Its
  ordered user-entry index follows the projection's changed tail. A single
  widget draws only the ticks that fit, handles hover and wheel input locally,
  and shapes a bounded preview only when its source changes. A narrower pane
  hides the rail without changing the transcript's padding or scroll state.
  To compare hover and wheel CPU work at 200 and 20,000 entries, run
  `just test prompt_rail_hover_and_wheel_cost -- --ignored --nocapture`.
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
  Selection-copy resolves logical positions, without full message strings embedded in every tree.
- Selection indexes and decoration geometry belong to mounted paragraphs, not
  the entire history. Inside a long assistant/reasoning entry, a second height
  index mounts top-level Markdown items around the same scroll viewport; only
  their image URLs are requested. Sparse numeric height measurements can outlive
  the mounted entry within the selected session. When switching away from a
  scrolled-up chat, Orbit keeps an in-memory bookmark for the visible row,
  including its offset and any measured Markdown item heights. Returning restores
  that reading position; chats left at the bottom still follow new messages.
  Copy still walks the complete source. A single giant paragraph, code fence or nested structure can still
  be expensive to lay out; see the [long-reply measurements](benchmarks/session-and-rendering.md#layout-within-a-long-markdown-reply).

## Conversation Find

Ctrl+F searches the loaded transcript, not the sidebar catalog, hidden activity,
or files on disk. Literal Unicode case-insensitive matches use rendered Markdown
text (including code lines) and literal user text. Find retains a plain-text
projection only while open. A cancellable background scan coalesces edits;
unchanged entries share their results during streaming. Only the tail reply
keeps an incremental Markdown parser. A changed reference can invalidate earlier
blocks in that reply. Match ordinals use cumulative counts rather than walking
the history for each Next/Previous.

A jump mounts the target entry and Markdown item directly, then uses shaped glyph
bounds to reveal the occurrence vertically and, in wide code or tables,
horizontally. This does not expand activity groups or shape intervening replies.

To measure the search worker, run
`nix develop --command cargo test --release --locked -p orbit find_20k_entries -- --ignored --nocapture`.
The fixture contains 20,000 short formatted replies with code. In one local run
the first query took 51 ms, a different query over the cached text took 6 ms,
and updating one reply took 20–26 µs. These are index timings, not end-to-end
input or frame latency. A single very large, unfinished Markdown block can still
require reparsing that block when text arrives.

## Background work

- Composer imports queue file paths and prepare one image at a time on a blocking
  worker, outside the session I/O queue. Decode limits and atomic media writes
  are unchanged. A switch or autosave need not wait for PNG encoding. Results
  publish in input order to the originating draft. Pressing Send during an
  import waits for all of that draft’s current imports. Editing, adding or
  removing an attachment, leaving the composer, or an import failure cancels
  that send without discarding the draft. Quit waits for pending publication. Up to two clipboard offers
  are admitted at once because, unlike file paths, they can already hold bytes.
  See the [two-import selection probe](benchmarks/session-and-rendering.md#selection-during-image-import).

- Shell output uses a bounded ring/deque instead of shifting its 1 MiB tail.
  Non-PTY pipes and child waits use Tokio. PTY reads and writes also run on
  Tokio, with readiness notifications from the shared async-io reactor and
  one blocking wait thread per PTY child. QuickJS cells and nested host calls
  retain dedicated worker threads, bounded by the existing per-conversation limits.
  Waiting cells wake on completion, cancellation or their next timer deadline,
  not a 2 ms polling cycle. Output/store size accounting is incremental.
- Git status refreshes on project selection and every five seconds for the
  selected session. Captures coalesce per project, run off the UI thread and
  time out after 15 seconds. Unchanged untracked files reuse cached line counts.

## Measurements

See [session and rendering benchmarks](benchmarks/session-and-rendering.md) for
reproduction commands, fixture sizes, before/after results, and measurement limits.
