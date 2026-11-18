# Plan: WebSocket-only Responses in Pico V2

**Decision:** replace the provider's HTTPS POST/SSE transport for all Responses
requests with secure WebSockets. Full input and incremental input are two forms
of the same `response.create` message. There is no transport switch, SSE fallback,
or compatibility setting. OAuth, web search and other non-Responses endpoints
are outside this migration. The durable local conversation remains authoritative;
keep `store: false`.

**Status:** Implemented and covered by local WebSocket fixtures. Live subscription
endpoint acceptance and payload/latency measurement remain to be done. The HTTP
GET upgrade uses the normal TLS/proxy client; no Responses payload is POSTed.

## Implementation order

1. **Establish full-input parity first.** Connect to
   `wss://chatgpt.com/backend-api/codex/responses` with the OAuth, account,
   session and routing headers and the WebSocket beta header from the
   [pinned Codex client](https://github.com/openai/codex/blob/0d07dae8c9f7b0cd390aa4340fc1376b9d7c2e33/codex-rs/core/src/client.rs).
   Verify the handshake and a full request on the subscription endpoint. Feed
   WebSocket JSON events to the existing `ResponseStream` in
   [provider.rs](../crates/pico-core/src/provider.rs); preserve tool dispatch,
   final item reconciliation, usage, cancellation and timeouts. Use this path
   for agent turns, titles, native compaction and fallback summaries. Titles
   and compaction use independent full-input requests so they cannot advance the
   agent's chain. Migrate Responses test fixtures and remove the Responses POST
   and SSE decoder rather than maintaining two implementations.
2. **Add connection-local continuation.** Reuse one socket across tool rounds
   and user turns for each active conversation, with one request at a time;
   parent and child sessions never share sockets. Evict idle sockets. Only after
   a validated completion, checkpoint the nonempty response ID, logical input
   snapshot, ordered response output and non-input request properties in memory.
   If properties match and the current input begins with the previous input
   **plus its output**, send only the suffix with `previous_response_id`.
   Otherwise send full current input without that field. Send instructions,
   tools and model settings in both modes. Choose the suffix using logical
   history **before** calling `media::project`, so unchanged images need not be
   read, encoded or uploaded again.
3. **Recover without repeating work.** Discard the checkpoint on socket loss,
   cancellation, restart or auth ownership change. Reconnect with current
   credentials and full input on the next request. Compaction or a changed
   history/settings prefix starts a new chain. An explicit rejection of a
   previous response ID before output permits **one** full-input retry over
   WebSocket. Keep bounded retries for definite setup failures; if the upgrade
   remains blocked, explain how to check the connection or proxy.
4. **Recover a dropped agent stream automatically, at the agent-loop boundary.**
   Treat a transport interruption before validated completion as a distinct
   outcome, not a reason for the provider to resend the original request.
   Show one chat notice, e.g. “Connection lost. Pico is reconnecting to continue
   from saved progress (attempt 1 of 3).” Update that notice on later attempts;
   do not send it to the model as user input.
   - Stop dispatch for the old response. Let an already-started tool invocation
     finish or yield before reconnecting; Stop still cancels it. Commit
     validated completed items and saved tool results in order. Never execute
     partial arguments or redispatch an old call. Mark unfinished calls as
     interrupted; if a side effect cannot be accounted for, pause and tell the
     user what to inspect rather than guessing. Do not retry failed storage or
     reconciliation as if it were a network failure.
   - After repair is durable, open a fresh socket and issue a new full-input
     request from the recovered session, without a previous response ID or a
     duplicate user message. Incomplete streamed text is not a completed
     assistant message; distinguish it in chat from the new answer. This is a
     new generation, not resumption at a token boundary.
   - Use bounded, cancellable backoff (three recovery attempts per
     user turn). Stop cancels pending waits, connection setup and generation.
     Do not auto-continue after explicit cancellation or application restart.
     On success update the notice; on exhaustion retain progress and offer
     manual retry. Share the attempt budget across reconnects so repeated drops
     cannot reset it.

## Acceptance checks

- Record frames for an image prompt, a tool result and a later user message.
  The first frame contains the image; the next two name the immediately preceding
  responses and contain only the new items. A new image is uploaded when added.
  Compare payload bytes on a live image-heavy conversation without logging content.
- Exercise reconnect, stale-ID rejection and compaction: the next full request
  contains the complete *current* input, without an ID, over WebSocket. Confirm
  a later compatible request can use a delta again.
- Run overlapping parent, child and title requests and verify isolation. Verify
  a failed upgrade never triggers an HTTP POST.
- Drop the socket after partial text, during tool arguments, and after a
  persisted tool result. Verify the notice, automatic full request built from
  repaired input, no duplicate user message or tool invocation, and successful
  completion without user action. Contrast an uncertain side effect: expect a
  pause, not automatic re-execution.
- Repeated drops exhaust the turn's budget. Stop during backoff prevents the
  next request. Neither case loses saved progress.

Preconnection and speculative request prewarming are not required for this plan.
