# Responses transport

Pico sends Responses requests over TLS WebSockets to
`wss://chatgpt.com/backend-api/codex/responses` with `store: false`. There is no
HTTP POST/SSE fallback. If the upgrade is blocked, check the connection and
proxy. OAuth and web search use separate endpoints.

Each active conversation has its own socket with one request in flight.
After a validated completion, the client keeps the response ID, logical input,
output and request properties in memory. If the next input extends that input
**and output**, and the properties still match, the next `response.create`
includes `previous_response_id` and only the new items. Full requests start
with Pico’s developer message; continuations inherit it rather than resending
it. Tools and settings still travel with each request. Pico chooses the suffix before reading
and encoding media, so unchanged images are not uploaded again.

At turn start Pico reads `AGENTS.md` directly from the selected project directory.
It sends the file as user-role project guidance, separate from Pico’s own
instructions. If the file changes, Pico appends the new guidance; if removed or
emptied, it appends a removal notice. Unchanged guidance is not appended again.
After compaction, only the latest guidance item is carried forward explicitly.

A changed prefix, compaction, socket loss, cancellation, restart or account
change invalidates continuation. The next request sends full current input.
An explicit stale-ID rejection before output permits one full-input retry.
Titles and compaction use separate full-input requests and cannot advance the
agent's chain. For a drop *during* generation see
[tool execution and recovery](code-mode.md#early-execution-and-recovery);
the provider does not simply resend a request that may have caused side effects.

Local WebSocket fixtures cover these paths. To check live payload savings with
an authorized subscription, compare frame sizes for an image prompt, its tool
result and a later user message without logging content. Only the first frame
should upload the original image; a new image must still be uploaded when added.

## Reasoning effort

The first turn sets the request-level effort for the model. Changing effort on a
later turn appends a `configuration_update` before the next user message; it
does not change the request-level effort or force a fresh WebSocket. A full
replay sends the updates in their original positions. Compaction carries the
latest update into the new window. Switching models starts a new baseline so
model-specific efforts (such as Luna's None) do not leak into the next model.

The opt-in check below sends two tiny synthetic prompts on one real subscription
WebSocket per catalog model. It uses no project history and does not refresh or
modify the saved login. Run only with authorization to consume subscription usage:

```sh
PICO_EFFORT_ACCEPTANCE_AUTH="${XDG_CONFIG_HOME:-$HOME/.config}/pico-evo/auth.json" \
  nix develop --command cargo test --locked -p pico-core live_configuration_update -- --ignored --nocapture
```

The [reasoning guide](https://developers.openai.com/api/docs/guides/reasoning#change-reasoning-mid-conversation)
specifies that the response echoes the request-level effort, not the effective
effort. This check verifies acceptance and continuation, not a speedup.
