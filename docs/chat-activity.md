# Chat activity presentation

The transcript presents activity rather than the code-mode protocol. Consecutive
reasoning, tools and subagent completion observations form a quiet activity group.
Assistant prose, user messages and other harness notices remain boundaries.

Select transcript text and use Ctrl+C to copy, including across messages and
offscreen tool rows. Double-click selects a word; triple-click selects a paragraph
or a logical line in code and tool output. Drag to extend by the same unit.

- Groups start collapsed, including during live work. Opening or closing a group
  is a user choice and remains stable as results arrive. While working, the left
  disclosure slot shows the footer's animated status circle, using the same clock;
  hovering anywhere on the group header
  reveals the right/down chevron. Finished groups always show the chevron.
- Completed groups summarize actual tool-call counts and delegation counts by
  explicit profile (for example, “1 review · 2 explore”). Live
  groups show the current tool/input or reasoning heading, with completed/running
  counts kept separate from the ellipsized preview. Completed counts include both
  successful and failed calls. The failed count appears only for groups with more
  than 10 actual tool calls and more than 50% of those calls failed (running calls
  remain in the denominator). Hidden code-mode wrappers and subagent completion
  notices do not contribute to these counts. Individual failures remain visible
  in expanded details; failures never automatically expand the group.
- Expanding shows compact status/name/input rows. Shell previews use the actual
  `cmd`, not a generated description. Polls remain individual `write_stdin` calls;
  nonempty stdin is explicitly identified as input rather than polling.
- Successful `exec` and `wait` wrappers with nested activity are secondary
  **Execution details**. Opening that disclosure inserts wrappers in their
  original chronological positions; expanded nested tools retain their parent
  execution reference. Failed wrappers and executions without nested tools remain
  primary rows. Waits use durable cell IDs to find the originating execution.
- Consecutive reasoning entries share a disclosure showing the latest actual
  Markdown/bold summary heading (or a text preview). Expanding retains all original
  bodies, in order, with their original selection identities. Streamed summary
  parts retain newline boundaries, just like completed responses; a new partial
  heading does not replace the previous complete heading.
- Subagent completion observations use their durable run identity to display
  profile, title and terminal status. Expanded details retain the full harness
  notice and an Open subagent action. Other warnings and the questionnaire panel
  are not hidden by this presentation.

All tool rows use a solid status dot: yellow while running, green on success,
and red on failure or interruption.

The `exec_command` status dot tracks the **process**, independently of tool-call
completion: yellow while running, green on exit zero, and red on failure or
cancellation. Yielding a session ID does not turn the dot green. Exit updates
arrive automatically, without a `write_stdin` poll, and survive session switches.
After an application restart, previously running commands show red/interrupted
because their runtime tracking cannot be restored. Original tool output is retained.
Other rows still describe tool-call status; polls remain separate calls. Status
uses structured lifecycle events, never guesses based on command output text.

## Session attention

A stopped turn's **Needs attention** indicator is an unacknowledged notification,
not a permanent error state. This applies to all stopped-turn errors, including
provider flags and failures, and to restart interruptions.

- Opening/revisiting a session acknowledges its existing terminal attention and
  returns it to the normal idle grouping. A failure arriving while the session is
  already selected stays visible until Escape or revisiting the session.
- Escape acknowledges a stopped session. While work is active, it retains its
  Stop behavior; menus, dialogs and panels retain their existing Escape priority.
- Viewing a pending questionnaire does not dismiss it or acknowledge it away.
- Acknowledging a parent also acknowledges its existing child-failure alerts.
  New failures remain unacknowledged. Child failure/interruption labels, complete
  diagnostics, results and transcript entries remain intact.
- Acknowledgements persist across restart. They never retry or resume work.

## Expanded tool details

Shell calls show the decoded **Command**, including real line breaks and
indentation, followed by labeled **Arguments** when present. Other tools show
pretty-printed JSON, or the original input when it is not valid JSON (including
incomplete streamed input). **Raw input** discloses the original serialized input
at the beginning of the same virtual viewport and returns it to the top. Closing
the disclosure restores the formatted-only view. Execution references are muted,
selectable footer metadata, not the first line of the panel.

Formatting is presentation-only. Original input/output is unchanged in storage;
output whitespace and disclosed raw-input bytes remain intact in selection/copy.
The clipboard reconstructs the same visible document even after its rendered cache
is evicted. Changing the raw-input disclosure invalidates selections in that entry
because their logical byte offsets change. Raw disclosure state survives viewport
eviction, just like the tool's expansion state.

## Data and virtualization

`orbit-app/src/activity.rs` derives display rows from the original history. It does
not rewrite transcript entries, tool parents, model-facing output or notice text.
Synthetic activity/execution/thinking headers have distinct identities from source
rows. The virtual list retains row measurements and scroll anchors by identity.
Full Markdown/tool-detail caches are only prepared for visible source rows;
collapsing a group releases its output caches without forgetting manual expansion.
Chat prepares the actual viewport plus a 120px margin above and below, including
prose and expanded details. A transcript-local observer reports the actual scroll
viewport and effective offset even when content fits; it does not rely on iced
scroll notifications, which omit non-overflowing content. Geometry is refreshed
after session/reflow epochs and changes, without publishing on unchanged frames.
The sidebar keeps its larger overscan margin.
Display preview shortening never changes the stored inputs, output or selection-copy data.

Updates use persistent-history diffs and reproject the affected block/tail, not
all prior prose. Exceptionally large individual activity groups still require
rescanning that group when it changes; their streaming performance has not been
separately benchmarked.
