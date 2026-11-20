# Responses transport

Pico sends Responses requests over TLS WebSockets to
`wss://chatgpt.com/backend-api/codex/responses` with `store: false`. There is no
HTTP POST/SSE fallback. If the upgrade is blocked, check the connection and
proxy. OAuth and web search use separate endpoints.

Each active conversation has its own socket with one request in flight.
After a validated completion, the client keeps the response ID, logical input,
output and request properties in memory. If the next input extends that input
**and output**, and the properties still match, the next `response.create`
includes `previous_response_id` and only the new items. Instructions, tools and
settings still travel with the request. Pico chooses the suffix before reading
and encoding media, so unchanged images are not uploaded again.

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
