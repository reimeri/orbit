# Pico V2 and Codex: agent-loop and harness differences

This comparison describes Pico V2's current implementation and the **public
OpenAI Codex source**, not proprietary hosted Codex behavior. Codex references
are pinned to revision
[`0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33`](https://github.com/openai/codex/tree/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33),
inspected on 2026-11-16. Upstream behavior depends on model support, feature flags,
provider capabilities and configuration; supported paths are not universal defaults.
This is a source comparison, not a performance benchmark or a commitment to copy
all Codex features.

## Shared architecture

Both harnesses follow the same basic loop:

```text
Collect input -> request a model response -> execute tools
             -> append results -> request another response or finish
```

Pico already implements streaming responses, code mode, queued steering,
automatic compaction, cancellation and asynchronous subagents. Code mode is not
a missing architectural change: Pico exposes `exec` and `wait` as top-level
Responses tools, with built-ins available through JavaScript's `tools` object.

## Loop behavior

| Area | Pico V2 | Public Codex |
| --- | --- | --- |
| Tool dispatch | Waits for the entire model response to complete, then processes top-level tool calls sequentially in response order. | Can dispatch completed tool calls while the response is still streaming; tools may run concurrently or exclusively according to their capabilities. |
| Steering | Saves messages accepted during a turn and consumes them at the next loop boundary, after the current response and its returned calls. Does not preempt the active model request. | Has sampling interruption/draining and follow-up machinery for incoming messages; behavior depends on the execution path. |
| Recovery | Retries connection/setup failures and selected HTTP errors. A stream interruption ends the turn with a recoverable error rather than automatically retrying generation. Incomplete tool calls are not executed. | Has stream-level retry and transport fallback, while treating some provider outcomes as terminal rather than retryable. |
| Transport | Uses HTTP/SSE and sends the current accumulated input context on each request. | Also supports persistent WebSockets, preconnection, request prewarming and incremental continuation. |
| Compaction | Pauses the loop at a reported-usage threshold, or after a context-limit error. Tries native encrypted compaction, with bounded chunked summarization as fallback. | Includes an asynchronous/background compaction path in addition to foreground compaction machinery. |

**Sequential dispatch does not mean Pico has no concurrency.** A JavaScript cell
can run nested calls with `Promise.all`, yielded cells can remain running, shell
sessions can outlive a call, and subagents run asynchronously. The distinction is
how separate top-level calls from one model response are dispatched. Pico currently
advertises `parallel_tool_calls: true` but processes those calls sequentially.

Pico saves tool-call state before execution. Recovery pairs unfinished calls with
an explicit interrupted result rather than blindly repeating potentially completed
side effects. Stop cancels ongoing work; it does not undo completed commands.
Fallback compaction preserves historical context and oversized output references
rather than treating a summary as the only surviving copy.

Implementation references: Pico's [agent loop](../crates/pico-core/src/agent.rs),
[provider](../crates/pico-core/src/provider.rs) and [code-mode contract](code-mode.md);
Codex's [turn execution][codex-turn], [tool scheduling][codex-parallel],
[retry handling][codex-retry] and [background compaction][codex-compaction].

## Tool discovery

### Pico: a complete built-in catalog upfront

Pico constructs its nested tool catalog from built-in definitions, filtered by
conversation capabilities such as web policy and main-versus-child status. That
same catalog supplies:

- Descriptions and generated TypeScript signatures inside `exec.description`.
- The callable JavaScript `tools` object.
- `ALL_TOOLS`, including input and output schemas.
- Availability checks before nested tool execution.

The model therefore already has documentation for every available nested tool:

```js
text(await tools.exec_command({cmd: "git status"}));
```

`ALL_TOOLS` provides introspection over that existing list; it does not discover
additional integrations. Pico has no `tool_search`, deferred-tool loading,
client-supplied dynamic tool registration, or extension registry. Its schema
renderer supports the shapes used by the built-ins rather than arbitrary external
tool schemas.

See [catalog construction](../crates/pico-core/src/code_mode/catalog.rs),
[execution checks](../crates/pico-core/src/code_mode.rs) and
[JavaScript bindings](../crates/pico-core/src/code_mode.js).

### Codex: immediate and deferred tool exposure

Codex assembles a registry that can include built-ins, MCP/app integrations and
client-provided dynamic tools. Depending on configuration, some tools are
advertised immediately and others are deferred, avoiding full documentation for
every integration in the initial prompt.

When model support and eligible deferred tools permit it, Codex exposes
`tool_search`. The model can search for a capability, receive matching tool
definitions, then invoke a selected tool. This also works with nested code-mode
calls. Codex's code-mode description assembly distinguishes ranked-search and
catalog discovery paths; discovery is not synonymous with code mode, and not
every Codex configuration requires a search before calling tools.

```text
User asks for an issue-tracker lookup
    -> model searches available tool metadata
    -> harness returns matching definitions
    -> model invokes the selected tool
```

Searching does not create an integration or bypass its permissions: the tool must
already be registered and eligible. Dynamic tool registration is a separate
capability from searching registered tools.

See Codex's [registry/exposure assembly][codex-specs] and
[search handler][codex-tool-search]. With Pico's small built-in toolbox, upfront
documentation is reasonable. Deferred discovery is principally a toolbox-scaling
feature, not a prerequisite for using Pico's existing tools.

## Preconnection and request prewarming

These are distinct operations in the [Codex client][codex-client]:

| Operation | Work performed |
| --- | --- |
| Preconnect | Opens a WebSocket connection ahead of use, without sending prompt payloads. |
| Request prewarm | Sends `response.create` with `generate: false`, preparing request state without generating assistant output. |

A request prewarm can submit already-known instructions, tools and input. It
returns a response ID that a later generation can continue from:

```text
Preparation: open connection -> submit known state with generate:false -> ID W
Generation:  continue from W, adding new input -> generate assistant output
```

This does not generate a speculative answer or execute tools. It moves setup work
earlier and can reduce time to useful output when preparation overlaps other work.
An extra warmup round trip is not automatically faster if nothing can overlap it.
Pico currently has neither a WebSocket preconnect nor this no-generation warmup
path. This does not imply that its HTTP client opens a fresh TCP connection for
every request; HTTP connection reuse is separate.

See also the [OpenAI WebSocket guide][api-websocket].

## Incremental requests with `previous_response_id`

Let `H` be existing conversation input. The model returns response `R1`, containing
tool call `C1`, and the harness executes it to obtain result `O1`.

Pico sends accumulated context again:

```text
Request 1: input = H
Response R1: tool call C1
Request 2: input = H + C1 + O1
```

Codex's incremental path can instead send:

```text
Request 1: input = H
Response R1: tool call C1
Request 2: previous_response_id = R1, input = O1
```

The server combines the context associated with `R1`, including its output, with
the new tool result. **The logical conversation is the same; less history travels
over the wire.** These examples omit other request fields: required settings and
instructions still need to be supplied appropriately.

Codex verifies that the current input extends the previous known input and output,
and that relevant request properties remain compatible. If history was replaced,
shortened or otherwise changed, or relevant settings differ, it uses full input
instead of an invalid delta. Auth ownership changes invalidate cached transport
and continuation state.

### Benefits and limits

- Smaller repeated uploads and reusable server-side request state can reduce
  latency over many tool round trips. This does not itself change model reasoning
  or make tools execute remotely.
- Chaining does not provide unlimited context or remove the need for compaction.
- It does not make earlier context free. OpenAI's API documentation says previous
  input tokens in a response chain remain billable; prompt caching is a separate
  mechanism. This is not a prediction of ChatGPT subscription quota accounting.
- WebSocket continuation supports `store: false` through connection-local
  in-memory state; it does not require permanent response storage. If that state
  is unavailable, the client must recover with full context rather than relying
  on an unresolved response ID.
- The harness still needs its durable transcript for restart, recovery and replay.

See [Codex's incremental-request checks][codex-client],
[WebSocket state/recovery][api-websocket] and
[API conversation-state and billing documentation][api-state]. The API also
supports response-ID chaining over HTTP; it is not inherently a WebSocket-only
concept, although the connection-local low-latency path discussed here is.

## Other harness differences and implications

Pico implements its own fresh-context QuickJS runtime rather than embedding
Codex's runtime. Its shell is unrestricted: it has no Codex-style approval/sandbox
machinery. Pico's subagents are read-only **by instruction**, not filesystem
isolation. They use fixed profiles, isolated context, no nested delegation and
idle-child continuation; see [subagent contracts](subagents.md).

The most direct loop-behavior gaps are streaming/parallel top-level tool dispatch,
preemptive steering and automatic stream recovery. Prewarming and incremental
requests are primarily performance work. Deferred discovery primarily matters if
the tool ecosystem grows. These distinctions do not require replacing the basic
agent-loop architecture, and do not by themselves establish a model-quality gap.

[codex-turn]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/session/turn.rs
[codex-parallel]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/tools/parallel.rs
[codex-retry]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/responses_retry.rs
[codex-compaction]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/session/async_compaction.rs
[codex-specs]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/tools/spec_plan.rs
[codex-tool-search]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/tools/handlers/tool_search.rs
[codex-client]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/client.rs
[api-websocket]: https://developers.openai.com/api/docs/guides/websocket-mode
[api-state]: https://developers.openai.com/api/docs/guides/conversation-state
