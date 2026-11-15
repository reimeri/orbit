# Chat activity presentation

The transcript presents activity rather than the code-mode protocol. Consecutive
reasoning, tools and subagent completion observations form a quiet activity group.
Assistant prose, user messages and other harness notices remain boundaries.

User and assistant messages have no sender header or Copy button. The user card
border stays visible; messages use V1's 8px gap. User messages match V1's 12px
vertical padding, while assistant messages use 6px vertical padding. All chat
disclosures use font-independent right/down chevrons. Expanded tool, web and
subagent sections also omit Copy buttons; select transcript text and use Ctrl+C.
Settings and session-menu copy actions are unchanged.

- Groups start collapsed, including during live work. Opening or closing a group
  is a user choice and remains stable as results arrive. While working, the left
  disclosure slot shows the status circle; hovering anywhere on the group header
  reveals the right/down chevron. Finished groups always show the chevron.
- Completed groups summarize actual tool-call counts and delegation counts. Live
  groups show the current tool/input or reasoning heading, with completed/running
  counts kept separate from the ellipsized preview. Failure counts stay visible
  without automatically expanding the group.
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
  bodies, in order, with their original selection identities.
- Subagent completion observations use their durable run identity to display
  profile, title and terminal status. Expanded details retain the full harness
  notice and an Open subagent action. Other warnings and the questionnaire panel
  are not hidden by this presentation.

A status describes a **tool call**, not a consolidated process lifecycle. A shell
call that returns a running process ID has returned successfully; later polls are
still separate calls. Nonzero process exits, native tool errors and code-execution
errors carry explicit failure metadata, not guesses based on output text. No
elapsed-time metadata or polling consolidation is introduced here.

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

The compact disclosure row keeps identical geometry when opened or closed. Its
padded, slightly indented detail card appears **below** the row, rather than adding
padding around the header. Tool names use stronger text; section headings use a
small semibold UI font, while commands, arguments and output remain monospace on a
quiet inset surface.

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

`pico-app/src/activity.rs` derives display rows from the original history. It does
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

## Verification

`nix develop --command just verify` covers grouping boundaries, flattened calls,
failed wrappers, wait/cell linkage, multiple reasoning headings, selection/copy,
streaming outcomes, persisted details, and virtual-list measurement retention.
Layout/event regressions cover fitting tool-group expansions, viewport resizing,
clamped offsets after overflow disappears, equal-geometry session changes, wheel
scrolling, and unchanged-frame notification suppression.
The existing long-transcript and bounded-output tests also remain in place.

Layout and clipboard regressions cover stationary headers at narrow/wide widths,
header hit targets, decoded commands, formatted JSON, raw-input toggling, and exact
Unicode/whitespace copying after viewport eviction.

The rendered-window smoke includes collapsed/live activity, expanded calls,
merged reasoning, execution details and a failed multiline command with arguments
and disclosed raw input at constrained chat width.
It also retains image/web detail fixtures. Use `just smoke-gpu` from a Wayland
desktop for the native GPU path.

Manual acceptance with a live conversation:

1. Run parallel commands and a review subagent. Check the collapsed live summary
   and completion counts, then expand directly into the actual tools.
2. Run a command that exits nonzero, a JavaScript error before any nested tool,
   and an error after a successful nested tool. Keep the group collapsed and
   confirm the failure count remains visible; inspect all original output.
3. Yield a code cell and call `wait`. Check that successful observations appear
   under Execution details rather than inflating the actual-tool list.
4. Expand consecutive thinking, select text across its original bodies, and copy.
   Select older assistant prose while new activity arrives; selection should stay.
5. Check a completed subagent's compact profile/title/status, full notice and
   navigation action, including after restart and after continuing that child.
