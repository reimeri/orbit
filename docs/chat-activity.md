# Chat activity presentation

The transcript presents activity rather than the code-mode protocol. Consecutive
reasoning, tools and subagent completion observations form a quiet activity group.
Assistant prose, user messages and other harness notices remain boundaries.

- Groups start collapsed, including during live work. Opening or closing a group
  is a user choice and remains stable as results arrive.
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

## Data and virtualization

`pico-app/src/activity.rs` derives display rows from the original history. It does
not rewrite transcript entries, tool parents, model-facing output or notice text.
Synthetic activity/execution/thinking headers have distinct identities from source
rows. The virtual list retains row measurements and scroll anchors by identity.
Full Markdown/tool-detail caches are only prepared for visible source rows;
collapsing a group releases its output caches without forgetting manual expansion.
Chat prepares the actual viewport plus a 120px margin above and below, including
prose and expanded details. The sidebar keeps its larger overscan margin.
Display preview shortening never changes the stored inputs, output or Copy data.

Updates use persistent-history diffs and reproject the affected block/tail, not
all prior prose. Exceptionally large individual activity groups still require
rescanning that group when it changes; their streaming performance has not been
separately benchmarked.

## Verification

`nix develop --command just verify` covers grouping boundaries, flattened calls,
failed wrappers, wait/cell linkage, multiple reasoning headings, selection/copy,
streaming outcomes, persisted details, and virtual-list measurement retention.
The existing long-transcript and bounded-output tests also remain in place.

The rendered-window smoke includes collapsed/live activity, expanded calls,
merged reasoning, execution details and a failed command at constrained chat width.
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
