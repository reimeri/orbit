# Async subagents

Pico's built-in subagents are persistent child conversations. Each assignment has
its own run ID; continuing a child retains its conversation but creates a new run.

| Type | Default model | Default effort | Instructions |
| --- | --- | --- | --- |
| `review` | Sol | `medium` | Review changes for actionable issues, with evidence and file/line references |
| `explore` | Luna | `high` | Fast project inspection and precise answers with source references |

Fast is off by default. In **Settings → Models**, each profile has independent
model, effort and Fast controls. Saving affects the next assignment, including
one that resumes an existing child; assignments already running keep their settings.

Both profiles are **read-only by instruction, not by
filesystem isolation**: their shell commands run with the user's privileges.
They share the parent's workspace, but not its transcript, code-mode store,
runtime cells, shell sessions, or usage counters. New children receive project
`AGENTS.md`, profile instructions, and the explicit task briefing. Subagents
cannot delegate.

## Tools

Only `exec` and `wait` are top-level Responses tools. Main conversations have
three additional delegation tools on `tools`; children have shell tools, `view_image`,
and `web__run` when web access is enabled, but cannot generate images.
The global Live/Cached/Disabled web policy applies to both profiles. Children use
their own web conversation identity and profile model; pass source URLs, not
conversation-scoped web reference IDs, between agents.
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
  notices are delivered on the parent's next user-initiated turn. If an individual
  assignment record is malformed, startup identifies it in a warning and skips
  it without changing the row. Other assignments and conversations still open.

Session acceptance and assignment insertion share one SQLite transaction.
Terminal results are stored separately from transcripts. Notification IDs make
parent delivery idempotent across a crash between parent persistence and the
assignment's delivery acknowledgement. A failed terminal-result write retains
its complete result in memory. Sending to that parent retries its family's
results before accepting new input; other parents can send and finish work
independently. An explicit flush (including shutdown) retries every family and
reports any that still cannot be saved, without rerunning assignments. Keep Pico open if storage cannot be saved.

## UI

Open **Agents** on the right rail to inspect running and completed assignments.
Spawn rows link to the child and distinguish a successful spawn from the outcome
of the assignment. Select a child to read its transcript or stop it; **Back to
parent** restores the parent's draft and scroll position. Request continuations
through the parent; children have no composer or editable model selector.

Children do not clutter the main session list. Their parent remains in Working
while children run, and unacknowledged child failures/interruption contribute to
its attention indicator. Viewing the parent or acknowledging it with Escape clears
existing child alerts without changing their outcomes. Children and results remain
saved with the parent. Confirmed parent deletion atomically deletes the whole
family and is refused while related turns are active. Project files are never deleted by session deletion.

For focused scheduler and recovery tests, run
`cargo test --locked -p pico-core agent::subagents` inside `nix develop`.
