# Phase 1 code-mode contract

Only `exec` and `wait` are sent as top-level Responses tools. Shell tools are
exposed on the JavaScript `tools` object, not duplicated in the top-level catalog.

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
cells/turns in the current app process, not restarts. Image/audio helpers are outside
Phase 1.

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

Pico resource policy: 64 MiB JS heap and 1 MiB stack per cell, 16 MiB serializable
session store, 32 open cells, 64 concurrent nested calls and 64 shell sessions per
conversation. Waits are bounded to 300 seconds. Command output keeps a bounded
1 MiB tail with an explicit dropped-byte notice; returned output is further limited
by the requested approximate token budget. The app cancels process groups on Stop
and exit. Deliberately daemonized/re-parented processes can escape process-group
cleanup; this is not a sandbox or a process supervisor.

## Reference provenance

The top-level grammar/schema and execution semantics were checked against the
public OpenAI Codex source (Apache-2.0), retrieved during implementation:

- `codex-rs/core/src/tools/code_mode/execute_spec.rs`
- `codex-rs/core/src/tools/code_mode/wait_spec.rs`
- `codex-rs/code-mode-protocol/src/description.rs`
- `codex-rs/core/src/tools/handlers/unified_exec.rs`

Repository: <https://github.com/openai/codex>. Pico has its own Rust/QuickJS runtime;
it does not embed Codex/V8, Codex's remote execution service, approval/sandbox
machinery, or Phase 2 multimedia tools. No backwards-compatibility adapters exist.
