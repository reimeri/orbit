# Code-mode contract

Only `exec` and `wait` are sent as top-level Responses tools. Shell, web, media, questionnaire and subagent tools are
exposed on the JavaScript `tools` object, not duplicated in the top-level catalog.

Nested tool documentation lives in `exec.description`, not the system instructions.
Each entry has a one-line purpose and a TypeScript declaration generated from its
input and output JSON schemas in `crates/orbit-core/src/code_mode/catalog.rs`.
The same catalog supplies JavaScript `tools`, `ALL_TOOLS` (including both schemas),
and nested-call availability checks. Text-returning tools are
correctly declared as `Promise<string>`, even when the string contains JSON.

When adding a tool or changing its arguments/results, update its schemas
alongside its Rust handler. Field descriptions become TypeScript comments.

`exec` is an OpenAI **custom/freeform** tool using Codex's Lark source grammar.
It accepts raw JavaScript, optionally preceded by:

```js
// @exec: {"yield_time_ms": 10000, "max_output_tokens": 2000}
const results = await Promise.all([
  tools.exec_command({cmd: "pwd", login: false}),
  tools.exec_command({cmd: "git status --short", login: false})
]);
for (const result of results) text(result);
```

`wait` is a function tool with `cell_id: string` required and optional
`yield_time_ms: number`, `max_tokens: number`, `terminate: boolean`.
`additionalProperties` is false. Wait/yield defaults are 10,000 ms; output defaults
to 10,000 approximate tokens. Wait consumes only new output and closes a completed
cell. A terminal result is not retrieved twice.

Each execution is an async module in a **fresh embedded QuickJS context**, not
Node and not a persistent JavaScript REPL. No direct filesystem, network or console
APIs are installed. The shell tools are deliberately unrestricted. Supported globals
include `text`, `exit`, `store`/`load`, timers, `ALL_TOOLS`, `notify` and `yield_control`.
`notify` injects an additional custom-tool output into the conversation;
`yield_control` yields the accumulated cell output while execution continues.
Unawaited promises do not keep a completed module alive. `store` values survive
cells/turns in the current app process, not restarts.

Nested shell inputs:

```ts
exec_command({
  cmd: string,
  workdir?: string, shell?: string, login?: boolean, tty?: boolean,
  yield_time_ms?: number, max_output_tokens?: number, timeout_ms?: number
}): Promise<string>

write_stdin({
  session_id: number, chars?: string,
  yield_time_ms?: number, max_output_tokens?: number
}): Promise<string>
```

Defaults: project working directory, `$SHELL` (or `/bin/sh`), login shell enabled,
TTY disabled, exec yield 10,000 ms, stdin yield 250 ms. A running command returns
a numeric session ID; empty `chars` polls it. TTY mode keeps input open and accepts
control characters such as `\u0003`. Non-TTY commands have closed stdin.

Orbit resource policy: 256 MiB JS heap and 1 MiB stack per cell, 16 MiB serializable
session store, 32 open cells, 64 concurrent nested calls and 64 shell sessions per
conversation. Waits are bounded to 300 seconds. Command output uses a
1 MiB in-memory spill threshold, spilling the complete unread output to a private file under
`/tmp` when needed. Returned output exceeding the requested approximate token budget
is likewise saved in full, not truncated. Results contain an absolute file path and
exact byte size, with no partial preview. A zero budget still returns a file reference
for nonempty output. Budgets use four Unicode characters per approximate token,
capped at 100,000 tokens. Non-UTF-8 command output is always saved byte-for-byte to
a file rather than decoded lossily. Each read consumes new output once; its file is
an immutable snapshot, not a growing log. Files survive command, cell, session and
app exit; Orbit does not clean them up. Read or search files using shell commands.
If saving fails, buffered data is retained and the error provides the session/cell
ID for retry after fixing disk space or permissions; do not rerun the command.
The app cancels process groups on Stop and exit. Deliberately daemonized/re-parented processes can escape process-group
cleanup; this is not a sandbox or a process supervisor.

## Early execution and recovery

Completed top-level calls start while the model response is still streaming. Orbit
waits for the complete output-item event, not just parseable argument text. Calls
execute in response order with at most one `exec`/`wait` invocation active. Missing
earlier output items hold later calls until their ordering is resolved. A yielded
cell releases the invocation slot while its background work continues; nested
`Promise.all` concurrency is unchanged. Unanswered questionnaires retain the
existing post-tool barrier.

Completed response items and tool results are journaled durably until the response
is committed to context. Calls are saved before execution, and results before the
next invocation. Final response reconciliation does not rerun early calls. Images
emitted by an earlier invocation are available to subsequent image-edit requests
even before the model response finishes.

On a dropped WebSocket, no more calls from that response start. A running
invocation can finish or yield; its result is saved before reconnecting. Calls
that never started get explicit not-executed results. Orbit opens a new socket
and requests a new response from repaired local history (at most three times
per turn). It does not resume partial text, redispatch old calls or bypass a
pending questionnaire. Stop still cancels the invocation and reconnection.
Provider protocol errors, unaccounted-for side effects, and storage failures
are not transport retries. Restart repair never automatically starts work.

Cell text buffers spill to `/tmp` after 4 MiB of UTF-8 (including inserted separators),
retaining all text. The outer `exec`/`wait` budget also uses complete-file references
for oversized output, independently of nested shell tool budgets. Large JavaScript
exception details use the same file-backed behavior. Empty emissions do not allocate
fragments. Waiting cells are event-driven; timer callbacks scheduled during a tick
run on a subsequent tick so resolved modules can finish. Tool-start persistence
must succeed before commands launch. Session switching can release transcript
bodies without discarding process/cell IDs, unread results or shared `store` values.

Fallback context compaction preserves oversized generated summaries in `/tmp`
instead of clipping them. It also archives the previous checkpoint and the new
transcript entries (including nested tools and notifications) so file references
remain accessible even if the summarizer omits them. The final continuation includes
these references and is itself saved to a file if it exceeds the inline budget.
Service error details also use complete-file references when oversized. If saving
one-shot diagnostics fails, Orbit reports the failure and returns the complete data
inline (hexadecimal for non-UTF-8 bytes), rather than silently discarding it.

## Other built-in tools

See [subagents](subagents.md), [questionnaires](questionnaires.md),
[web access](web-search.md), and [image tools](images.md) for their schemas,
permissions and lifecycles.

## Reference provenance

The top-level grammar/schema and execution semantics were checked against the
[public OpenAI Codex source](https://github.com/openai/codex) (Apache-2.0):

- `codex-rs/core/src/tools/code_mode/execute_spec.rs`
- `codex-rs/core/src/tools/code_mode/wait_spec.rs`
- `codex-rs/code-mode-protocol/src/description.rs`
- `codex-rs/code-mode-protocol/src/json_schema_types.rs`
- `codex-rs/core/src/tools/spec_plan.rs`
- `codex-rs/core/src/tools/handlers/unified_exec.rs`

The generated exec catalog follows Codex's code-mode-only description assembly,
checked at revision `b1573f102413e96a6481087922bf425231222dee`. Orbit does not need
Codex's deferred-tool discovery or general-purpose schema renderer.
