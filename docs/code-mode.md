# Code-mode contract

Only `exec` and `wait` are sent as top-level Responses tools. Shell, web, media, questionnaire and subagent tools are
exposed on the JavaScript `tools` object, not duplicated in the top-level catalog.

Nested tool documentation lives in `exec.description`, not the system instructions.
Each entry has a one-line purpose and a TypeScript declaration generated from its
input and output JSON schemas in `crates/pico-core/src/code_mode/catalog.rs`.
The same catalog supplies JavaScript `tools`, `ALL_TOOLS` (including both schemas),
and nested-call availability checks. Main requests include nine tools when web access is enabled; child
requests include only shell tools and `view_image`. Text-returning tools are
correctly declared as `Promise<string>`, even when the string contains JSON.
General assistant/project policies remain in system instructions.

The schema renderer handles only the shapes used by these built-ins (objects,
arrays, scalar types and enums); no general schema framework or dependency is
needed. When adding a tool or changing its arguments/results, update its schemas
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
cells/turns in the current app process, not restarts. Image/audio helpers and image
tools are now implemented; see [images](images.md) for schemas, billing, file
ownership, capability handling and media limits.

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

Pico resource policy: 256 MiB JS heap and 1 MiB stack per cell, 16 MiB serializable
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
app exit; Pico does not clean them up. Read or search files using shell commands.
If saving fails, buffered data is retained and the error provides the session/cell
ID for retry after fixing disk space or permissions; do not rerun the command.
The app cancels process groups on Stop and exit. Deliberately daemonized/re-parented processes can escape process-group
cleanup; this is not a sandbox or a process supervisor.

## Reference provenance

The top-level grammar/schema and execution semantics were checked against the
public OpenAI Codex source (Apache-2.0), retrieved during implementation:

- `codex-rs/core/src/tools/code_mode/execute_spec.rs`
- `codex-rs/core/src/tools/code_mode/wait_spec.rs`
- `codex-rs/code-mode-protocol/src/description.rs`
- `codex-rs/code-mode-protocol/src/json_schema_types.rs`
- `codex-rs/core/src/tools/spec_plan.rs`
- `codex-rs/core/src/tools/handlers/unified_exec.rs`

The generated exec catalog follows Codex's code-mode-only description assembly,
checked at revision `b1573f102413e96a6481087922bf425231222dee`. Pico does not need
Codex's deferred-tool discovery or general-purpose schema renderer.

Repository: <https://github.com/openai/codex>. Pico has its own Rust/QuickJS runtime;
it does not embed Codex/V8, Codex's remote execution service, approval/sandbox
machinery, or its extension/attachment infrastructure. No backwards-compatibility adapters exist.

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
one-shot diagnostics fails, Pico reports the failure and returns the complete data
inline (hexadecimal for non-UTF-8 bytes), rather than silently discarding it.

## Subagents

Main conversations additionally expose `tools.spawn_subagent`,
`tools.wait_subagent`, and `tools.list_subagents`. Children expose shell tools and `view_image`, but not generation, questionnaires
or delegation. See [subagents](subagents.md) for the complete
contract, profile defaults, final-only results and completion/Stop lifecycle.
A successful spawn creates an assignment independent of the JavaScript cell: it
continues after the cell completes or is terminated. Use the returned persistent
`run_id` with `wait_subagent`; it is not a process ID or a code-cell ID.

## Questionnaires

Main conversations expose `tools.ask_user` for required select/text questions.
The promise waits for user input without a questionnaire timeout. Cells may yield,
but the harness pauses model continuation until pending questions are answered or
cancelled; use `wait` to retrieve any remaining cell output after resuming.
Unawaited calls end with the cell. Children do not have this capability. Answers are ordered JSON strings; large results use complete-file
references. See [questionnaires](questionnaires.md) for the schema, limits, UI,
cancellation/restart behavior and acceptance checklist.

## Web access

Main conversations expose `tools.web__run` when global web access is enabled.
It returns `Promise<string>` and supports search, image search, open, click, find,
PDF screenshot requests, finance, weather, sports and time. Structured backend
metadata is persisted for the UI, never returned to JS or included in model-facing
compaction archives. See [web access](web-search.md) for the complete contract,
Live/Cached/Disabled policy, text-only screenshot limitation and acceptance results.
