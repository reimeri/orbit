# Pico V2 and Codex: agent-loop and harness differences

This comparison describes Pico V2's current implementation and the **public
OpenAI Codex source**, not proprietary hosted Codex behavior. Codex references
are pinned to revision
[`0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33`](https://github.com/openai/codex/tree/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33),
inspected on 2026-11-16. Upstream behavior depends on model support, feature flags,
provider capabilities and configuration; supported paths are not universal defaults.
This is a source comparison, not a performance benchmark or a commitment to copy
all Codex features.

## Loop behavior

| Area | Pico V2 | Public Codex |
| --- | --- | --- |
| Tool dispatch | Starts completed top-level calls while the response is still streaming, in response order. `exec` and `wait` invocations are exclusive; yielded cells can continue running. | Can dispatch completed tool calls while the response is still streaming; tools may run concurrently or exclusively according to their capabilities. Its code-mode `exec` and `wait` are exclusive. |
| Steering | Saves messages accepted during a turn and consumes them at the next loop boundary, after the current response and its returned calls. Does not preempt the active model request. | Has sampling interruption/draining and follow-up machinery for incoming messages; behavior depends on the execution path. |
| Recovery | Retries connection/setup failures, selected HTTP errors, and explicit overloads before generated output or completed items. A dropped agent stream can start a new generation from recovered history, up to three times. Incomplete tool calls are not executed. | Has stream-level retry and transport fallback, while treating some provider outcomes as terminal rather than retryable. |
| Transport | Uses WebSockets only for Responses; compatible follow-ups send the new input with a previous response ID. | Also supports persistent WebSockets, preconnection, request prewarming and incremental continuation. |
| Compaction | Pauses the loop at a reported-usage threshold, or after a context-limit error. Tries native encrypted compaction, with bounded chunked summarization as fallback. | Includes an asynchronous/background compaction path in addition to foreground compaction machinery. |

Pico's exclusive top-level dispatch matches the pinned Codex [exec][codex-exec]
and [wait][codex-wait] handlers' [executor default][codex-executor]. It does
not prevent concurrent nested calls, background shell commands or subagents.
For durability and recovery boundaries see [code mode](code-mode.md).
Upstream references: [turn execution][codex-turn], [tool scheduling][codex-parallel],
[retry handling][codex-retry] and [compaction][codex-compaction].

## Tool discovery

Pico advertises its entire capability-filtered catalog in `exec.description`;
`ALL_TOOLS` inspects it rather than discovering new integrations.
Codex can register MCP, app and client-provided tools. When configured for
deferred exposure, [tool search][codex-tool-search] returns definitions for
already registered, eligible tools. It neither installs tools nor bypasses
permissions. See [upstream catalog assembly][codex-specs].

## Transport

Pico opens connections on demand. The [Codex client][codex-client] can also
**preconnect** (open the socket without sending a prompt) or **prewarm** (send
`response.create` with `generate: false`, then continue from its ID). Prewarming
does not generate an answer or run tools.

Both clients can send only new input with `previous_response_id`; this saves
upload bytes, not context window space. Pico's [continuation checks](responses-websocket.md)
allow connection-local reuse with `store: false`. Response-ID chaining is also
possible over HTTP. Prior input tokens in an API chain remain billable; that is
not a prediction of subscription quota accounting. See the [WebSocket guide][api-websocket]
and [API conversation state][api-state].

[codex-turn]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/session/turn.rs
[codex-parallel]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/tools/parallel.rs
[codex-retry]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/responses_retry.rs
[codex-compaction]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/session/async_compaction.rs
[codex-specs]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/tools/spec_plan.rs
[codex-tool-search]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/tools/handlers/tool_search.rs
[codex-client]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/client.rs
[api-websocket]: https://developers.openai.com/api/docs/guides/websocket-mode
[api-state]: https://developers.openai.com/api/docs/guides/conversation-state

[codex-exec]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/tools/code_mode/execute_handler.rs
[codex-wait]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/tools/code_mode/wait_handler.rs
[codex-executor]: https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/tools/src/tool_executor.rs
