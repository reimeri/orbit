# Long-session and large-library performance

Target workloads: **10,000 saved sessions, 20,000 transcript entries in a selected
session, and eight active conversations**. These are evaluation targets, not hard
limits. Execution limits remain per conversation; no global admission cap was added.

## Session storage and loading

- `sessions.sqlite3` is the session store. Metadata, ordered transcript records,
  and ordered provider context commit in one SQLite transaction (WAL mode,
  `synchronous=FULL`). Persistent tree diffs update only changed records. A draft
  update does not serialize or rewrite historical entries or provider input.
- A serialized UI I/O worker performs loading, editing, deletion and catalog/draft
  saving. Stop requests (including Escape) use the same queue: cancellation may
  wait for an earlier filing or send, but the window remains interactive.
  Conversation persistence has its own ordering lock; it snapshots state
  before writing and does not hold the conversation state lock across disk I/O.
  Agent async tasks offload saves to blocking workers. Message acceptance commits
  before it is published; tool-start boundaries commit before commands launch.
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
  historical assignment details on the storage worker, not while drawing.
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

For file locations, backup and format compatibility, see
[State and recovery](../README.md#state-and-recovery).

## Rendering

- Chat and sidebar construct rows only around their viewports. A Fenwick height
  index supports logarithmic offset lookup and measurement updates. Transcript
  append is incremental; full visibility rebuilding is reserved for group changes.
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
  publish in input order to the originating draft; Send waits for that draft’s
  imports, and Quit waits for pending publication. Up to two clipboard offers
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
