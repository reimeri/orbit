# Async subagents

Pico's built-in subagents are persistent child conversations. Each assignment has
its own run ID; continuing a child retains its conversation but creates a new run.
There is no profile loader, extension mechanism, context-sharing switch, or V1
compatibility layer.

| Type | Model | Reasoning | Instructions |
| --- | --- | --- | --- |
| `review` | `gpt-6.1-sol` | `medium` | Review changes for actionable issues, with evidence and file/line references |
| `explore` | `gpt-6-luna` | `high` | Fast project inspection and precise answers with source references |

Both profiles use Fast off. Their fixed model/effort choices do not follow the
main session's model preferences. Both are **read-only by instruction, not by
filesystem isolation**: their shell commands run with the user's privileges.
They share the parent's workspace, but not its transcript, code-mode store,
runtime cells, shell sessions, or usage counters. New children receive project
`AGENTS.md`, profile instructions, and the explicit task briefing. Subagents
cannot delegate.

## Tools

Only `exec` and `wait` are top-level Responses tools. Main conversations have
three additional nested tools on `tools`; children have shell tools and `view_image`, but cannot generate images.
Nested tools return strings. Subagent results are JSON text unless an output
budget requires a complete-file reference.

```ts
spawn_subagent({
  type: "review" | "explore",
  task: string,
  title?: string,
  session_id?: string
}): Promise<string>

wait_subagent({
  run_ids: string[],
  timeout_ms?: number,
  max_output_tokens?: number
}): Promise<string>

list_subagents({}): Promise<string>
```

`spawn_subagent` durably accepts the task before scheduling it and returns a
record including `session_id`, `run_id`, profile, title, assignment number, and
`status: "running"`. The child survives completion/termination of the spawning
JavaScript cell. Await spawning before ending a cell: unawaited JavaScript tool
calls have the same uncertain-outcome rules as other code-mode side effects.

Omitting `session_id` creates an isolated child conversation. Supplying it
continues only an **idle child owned by this parent, with the original profile**.
A busy child is rejected, not queued. A new assignment consumes no parent
history; include all relevant context in the task. Request identity is tied to
the persisted nested tool-call ID, so redispatching the same accepted call cannot
create a duplicate assignment. A newly issued tool call is a new request.

At most **six assignments per parent** run simultaneously. Other parents have
independent capacity; there is no additional app-wide subagent limit. Capacity
errors instruct the agent to wait for a child before trying again.

`wait_subagent` waits for **any** requested run to become terminal, or until its
timeout. It returns the status of each requested run and terminal results:

```json
{
  "runs": [
    {
      "run_id": "…",
      "session_id": "…",
      "status": "completed",
      "final_response": "The child's final response for this assignment."
    },
    { "run_id": "…", "session_id": "…", "status": "running" }
  ]
}
```

- Default timeout: 10,000 ms; capped at 300,000 ms. Zero polls immediately.
- Default output budget: 10,000 approximate tokens, using the existing lossless
  output mechanism. Oversized results are saved completely in private `/tmp`
  files with absolute paths and exact byte sizes, never truncated.
- Results can be retrieved repeatedly, including after continuing the same child
  or restarting Pico. Waits identify assignments by run ID, not by the latest
  activity in a session.
- Only the terminal response's final message text is returned. Commentary,
  intermediate responses, reasoning and tool outputs remain in the child
  transcript, not in the result.
- Successful empty final text is explicitly `"final_response": ""`.
- `failed`, `cancelled`, and `interrupted` runs return `error`, without a partial
  answer presented as final. Runtime wait cancellation does not itself cancel
  the child assignments.
- Unknown run IDs, or IDs owned by another parent, are rejected.

`list_subagents()` returns this parent's children, each with its latest assignment
and lifecycle status. It does not return final answers or full transcripts.

Example:

```js
const explore = JSON.parse(await tools.spawn_subagent({
  type: "explore",
  title: "Trace login refresh",
  task: "Read-only: trace refresh-token persistence. Identify relevant paths and failure handling. Do not change files."
}));
text(explore);
// Do independent main-agent work here, or in another cell.
text(await tools.wait_subagent({run_ids: [explore.run_id]}));
```

## Completion, Stop and restart

Completion or failure queues a small durable observation for the parent. A
running parent receives it at a response/tool boundary; an idle parent starts a
turn to acknowledge it and retrieve the result. Notices can coalesce, do not
include the full answer, and are not new user instructions. A parent's natural
completion does not cancel children.

- **Stop parent:** cancel the parent and its children, including retained child
  command sessions. Suppress automatic wake-up until a new user submission.
- **Stop one child:** cancel only that child and inform/wake its parent.
- **Switch session:** no cancellation and no focus stealing on completion.
- **Quit:** cancel all work and save it. Already executed commands are not undone.
- **Restart:** convert unfinished assignments to interrupted. Do not replay
  commands, resume assignments, or start parents for old notices. Retained
  notices are delivered on the parent's next user-initiated turn.

Session acceptance and assignment insertion share one SQLite transaction.
Terminal results are stored separately from transcripts. Notification IDs make
parent delivery idempotent across a crash between parent persistence and the
assignment's delivery acknowledgement. A failed terminal-result write retains
its complete result in memory; sending again or saving on exit retries storage
without rerunning the assignment. Keep Pico open if storage cannot be saved.

## UI

The robot-shaped **Agents** button on the right rail shows a small blue dot when
the current family has running children. The panel header shows the running count. Its panel shares the Settings slot and lists titles, profiles, statuses,
elapsed times and current runtime activity. Spawn tool rows link to the child
and show assignment status separately from the completed spawn call.

Each clickable card uses two lines: profile and live activity (or terminal status)
on the first; title and right-aligned model, effort and elapsed time on the second.
Long titles are ellipsized for display only. Clicking anywhere on the card displays
the child's live, full-width transcript with **Back to parent**
and **Stop**. The existing Markdown, selection, Copy and expandable tool views
are reused. There is no child composer or editable model selector. Parent drafts
and scroll positions survive navigation. Request child continuations through the
parent rather than messaging children directly.

Children do not clutter the main session list. Their parent remains in Working
while children run, and child failures/interruption contribute to its attention
indicator. Children and results remain saved with the parent. Confirmed parent
deletion atomically deletes the whole family and is refused while related turns
are active. Project files are never deleted by session deletion.

## Acceptance checks

Automated local-provider tests cover parallel work, independent context, profile
selection, cell-independent child lifetime, continuation ownership, immutable run
results, wait-any, six-child races, independent capacity across parents, terminal
failures/empty responses, Stop scope, coalesced notifications with user steering,
transaction rollback, terminal-write retry, lossless result budgets, restart
recovery, and family deletion. UI tests cover child read-only controls, sidebar
filtering, parent draft/scroll restoration, family deletion while inspecting a
child, and layouts at 1200×800 and 860×560. Software/GPU smoke views also render
the Agents panel and child inspection screen.

For live account acceptance in a disposable project:

1. Ask the parent to run independent Explore and Review tasks and continue its own
   work. Open Agents and verify parallel activity and the parent Working count.
2. Open each child, expand a command, copy its output, and return to the parent.
   Verify the parent's draft and scrolled position are preserved.
3. Let a child finish after the parent becomes idle. Verify the parent wakes,
   retrieves the final response, and does not receive intermediate child text.
4. Ask for a follow-up in the same Explore session. Verify retained child context
   and a new run ID; the old run's result remains retrievable.
5. Stop one child; verify its siblings continue and the parent acknowledges it.
   Then stop the parent; verify all children stop and no automatic wake follows.
6. Restart with saved results; verify results remain inspectable, no provider work
   starts automatically, and interrupted work requires an explicit continuation.
7. Delete a stopped parent while viewing a child. Confirm the deletion dialog
   mentions children and the UI leaves the deleted family.

Live OpenAI access is not exercised by the deterministic fixtures or smoke tests.
