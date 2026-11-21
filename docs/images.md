# Images and code-mode media

Orbit's built-in image tools use the calling session's workspace, never the currently
selected UI workspace.

## Attach images to chat

With the chat text field focused, press **Ctrl+V** (or **Shift+Insert**) to paste
an image. Drag one or more local image files anywhere into a main conversation
window to attach them to its current draft. PNG, JPEG, WebP and GIF are supported;
non-image files produce an error without discarding existing attachments. Text
clipboard content still pastes normally. Attachments are unavailable in read-only
child conversations, questionnaires and while a modal is open.

Thumbnails appear in a separate, left-aligned, horizontally scrollable row above
the text field. Each dropped file gets a tile immediately. An image paste gets
one as soon as Wayland confirms a supported image format, without waiting for
the clipboard bytes or decode; text pastes never show a tile. The active tile
shows only a spinner; later items show **Queued**. Each tile becomes
a thumbnail in place, or disappears if the import fails. Hover a thumbnail to
reveal **×**; click its image to open the existing full-size viewer (including
GIF playback). Removing a thumbnail removes it from the draft, never the original
file. Multiple images and image-only messages are accepted. Images sent while the
agent runs are queued as steering, just like text. Sent messages retain clickable
previews.

Unsent images survive session/project switches and restarts, and remain attached
when saving or sending fails. You can switch sessions during an import; its tile
and result belong to the original draft. Sending that draft and shutting down
wait for pending imports. Images are immutable private snapshots under the Orbit
configuration directory's `attachments/` folder; moving or deleting the source
file does not break a draft or sent message.
Snapshots are content-addressed and shared across drafts/history. They are retained
rather than removed on draft removal or session deletion, so shared references
cannot be broken; automatic orphan collection is not implemented.

The existing per-image media limits apply (79 MiB encoded, bounded pixels/animation
memory). The model receives prepared PNG images, not merely paths; GIF input is
its first frame, with the original saved path and first-frame limitation reported.
Attachment references remain in persistent context and fallback context recovery.

### Native input implementation and acceptance

The pinned iced/winit stack does not implement Wayland file drops or expose image
clipboard reads. See the [winit patch notes](../patches/winit/README.md).
It uses ordinary `wl_data_device` on the application's connection, not X11 or
clipboard-manager/data-control protocols. Transfers are nonblocking and time out
after ten seconds. File drops negotiate **Copy**, never Move.

Check native input on a Wayland desktop: paste both a screenshot and plain text,
then drop several files (including spaces or Unicode in their paths). Verify
the images keep their order and survive a restart before sending. Smoke fixtures
do not exercise transfers from another Wayland client.

## View

```js
image(await tools.view_image({path: "assets/screenshot.png"}));
```

`view_image({path: string})` returns `{image_url: string}`. Both main and child agents
can call it. It accepts local PNG, JPEG, WebP and GIF; paths may be relative or
absolute. Non-regular files (including FIFOs) are rejected without blocking.
There is no `detail` argument: Orbit uses the Codex unified image budget (original
transport detail, at most 6000 pixels per dimension and 10,000 32-pixel patches).
EXIF orientation is applied during preparation. The source file is never modified.
Model images are prepared as PNG; animated GIF model input is a **static first
frame**, explicitly reported to the agent, not an inspection of the animation.

## Generate and edit

```ts
tools.image_gen__imagegen({
  prompt: string,
  output_path: string,
  transparent_background?: boolean,             // false by default
  referenced_image_paths?: string[],              // up to 5 absolute paths
  num_last_images_to_include?: number,            // 1–5
  gif?: {
    frame_count?: number,                        // 2–16, default 9
    frame_duration_ms?: number                   // 20–2000, default 200
  }
}): Promise<{image_url: string; output_hint?: string}>
```

Generation is available only to the **main agent**. The schema follows Codex,
except that Orbit requires `output_path` instead of automatically choosing an
artifact destination. Unknown arguments are rejected.

```js
// @exec: {"yield_time_ms": 120000}
generatedImage(await tools.image_gen__imagegen({
  prompt: "A simple blue compass icon on a transparent background",
  output_path: "assets/compass.png",
  transparent_background: true
}));
```

- Omit both reference mechanisms to generate a new image. Supply references to edit.
- Explicit `referenced_image_paths` are absolute, following Codex's contract.
- Alternatively, use `num_last_images_to_include` for images actually emitted to
  the model in this session's current history. They are selected newest-first,
  then sent oldest-first. Nested UI previews and child-session images do not count.
  Complete/yield the emitting cell before selecting those images in another call.
  Compaction can remove images from current context; use their saved paths if so.
- Never provide both mechanisms. Too few available images is an actionable error.
- For GIF editing, inspect extracted frames and create a regular-grid spritesheet.
  Reference the spritesheet and supply `gif`; direct GIF references are rejected,
  including GIFs selected through recent history.
- Omit `gif` for PNG output, or supply `gif: {}` for the default animation.
- Do not print full results with `text` or `notify`: they contain base64. Use
  `generatedImage(result)`, which emits the image and the saved-path hint.

### Files and recovery

`output_path` must have `.png` without `gif`, or `.gif` with it. Relative paths
resolve against the calling workspace; absolute paths are allowed. The parent
must already exist and pass a write preflight. Existing files and symlinks, including dangling symlinks,
are **never overwritten**. Orbit-owned storage destinations,
including resolved parent aliases, are rejected.

Orbit holds the resolved parent directory open and publishes a complete staged file
with Linux `renameat2(RENAME_NOREPLACE)`. A competing destination writer wins
without being overwritten; a moved parent is rejected. Files are caller-owned:
session deletion never removes them. A crash can leave an uncommitted
`.orbit-image-*.tmp` in the destination directory.

Invalid arguments explain what to correct and are rejected before requests.
Generation can consume usage even if cancelled or if a connection fails. Orbit
never automatically retries uncertain image requests, HTTP 429/5xx, or API 401.
Only a definite Codex 401 gets one synchronized OAuth refresh/retry. There are
no redirects, model/quality downgrades, format fallbacks, or billing switches.

After receiving valid image bytes, Orbit saves a private recovery snapshot before
publishing the caller file. If publication fails or cancellation is observed before
publication, the error identifies those bytes: copy them to a new destination,
**do not regenerate**. Malformed/unexpected returned image bytes are preserved
under `/tmp` when possible, with a complete-data fallback if preservation fails.
A successful publication is never undone because of a later persistence failure.
Publication and logical session deletion share a lifecycle lease. If deletion wins,
received bytes are preserved outside the deleted session directory; the recovery
path therefore remains usable. Successful tool return data is built from the
received bytes, not by rereading a concurrently deletable preview.
Restart marks unfinished calls interrupted; it never replays a generation request.

## Settings and credentials

**Settings → Images**:

- Codex subscription (default): ChatGPT OAuth. The pinned public Codex image
  backend model is `gpt-image-2`; this is independent of the API model field.
- OpenAI API: official `/v1/images/generations` and `/v1/images/edits`, using the
  privately saved API key. API billing is separate from ChatGPT subscription usage.
- API model ID: one editable ID for generation and editing, initially
  `gpt-image-2.6-sunburst`. Reset explicitly adopts the current built-in default.
- Shared quality: `auto` (default), `low`, `medium`, `high`, `xhigh`, `max`.
  Values are sent unchanged. Backend/account/model support is not assumed.
- Size is automatic. Save applies settings; Cancel discards staged image changes.
  Each image call snapshots the saved settings; changing settings does not mutate
  a request already underway.

**Settings → Account** has a masked API-key entry, Save key, and Remove key.
Credentials are in private `image-api-key.json` alongside the application's
existing OAuth storage, not in `settings.json`, transcripts or model prompts.
There is no environment override. OAuth sign-out does not remove the API key.
Private files use mode 0600 and the application storage root uses mode 0700.

The model-update skill maintains `image_tools::DEFAULT_API_MODEL` from official
documentation. Updating that constant does **not** replace saved IDs, including
IDs that once matched the default. Subscription defaults must be checked against
Codex separately, not inferred from public API availability.

## Helpers, persistence and UI

- `image(url | {image_url} | ImageContent)` emits an image.
- `audio(url | {audio_url} | AudioContent)` emits audio for a capable chat model.
  PCM/float WAV clips shorter than 25 ms produce Codex's explanatory text notice.
- `content(block | iterable)` emits MCP text, image, audio, resource and resource-link
  blocks in order. Invalid blocks fail without discarding preceding emissions;
  encrypted MCP text is not accepted by this helper.
- `generatedImage({image_url, output_hint?})` emits image then optional hint.

Only inline base64 data URLs are accepted, not HTTP(S) URLs or filesystem paths.
`text`, `notify`, and existing text tools keep their text contracts. Current chat models accept images but **not native audio**:
`audio` preserves the artifact and reports a capability error instead of adding
unsupported content to model history.

JavaScript sees Codex-shaped result objects. Rust, SQLite and UI snapshots use
ordered typed text/media parts and private file references, not repeated base64
strings. Provider projection materializes bytes at the request boundary. Text
budgets still preserve complete oversize output under `/tmp`; they never stringify
or discard image/audio parts. Yields consume emitted content once. Fallback
compaction archives full transcript metadata and media paths independently of
model summaries.

Private originals, prepared images and thumbnails live in `media/<session-id>/`
under the existing **orbit storage root**. They survive restart and source-file
changes. Deletion releases media only when no session (including a fork)
still references it; compaction and transcript eviction do not remove media.
Failed cleanup after a committed deletion is reported without recreating the session.

Click a thumbnail to view the image at natural size, scaled down if needed
to fit the window. Hover for the path, resolution and byte size. Use Play/Pause
for GIFs; click the image or backdrop, or press Escape, to close. Closing the modal cancels
obsolete decoding and stops animation subscriptions. Decoding runs on workers,
with at most two UI decoders. Recently viewed decoded thumbnails can be reused
when scrolling back, without rereading the saved thumbnail file.

Resource policy: 79 MiB per media input, 10,240,000 decoded image pixels,
512 GIF frames / 256 MiB cumulative decoded animation data, 256 MiB JS heap,
16 MiB serializable store and 64 media emissions between yields. Two image-tool
calls can be active globally; waiting acquisition is cancellable. Image preparation
is serialized to bound decoded allocations. These are explicit Orbit resource
limits, not promises about every backend's acceptance limits.

## Markdown images in chat

Chat renders standard Markdown images such as:

```md
![Screenshot](assets/screenshot.png "Click to inspect")
![Local image](file:///home/me/Pictures/example.png)
![Remote image](https://example.com/image.webp)
```

Relative paths resolve against the **displayed conversation's workspace**, including
child conversations, not the process working directory or another selected project.
Absolute paths and local `file://` URLs are also supported. URL-encode spaces and
special characters in destinations, or use Markdown angle-bracket destinations for
paths with spaces. PNG, JPEG, WebP and GIF share the existing image validation limits;
SVG, data URLs and other source schemes are not supported.

HTTP(S) images load **automatically** when their message is mounted in the chat.
This contacts the image host (including any tracking URL in the Markdown); Orbit
sends no account credentials or browser cookies. Downloads have a 30-second timeout,
at most five redirects and the same 79 MiB input limit, including responses without a
Content-Length header. Embedded URL credentials and non-HTTP(S) redirects are rejected.

Previews fit the chat column and show the image's alt text beneath them. The optional
title appears on hover. Click a preview for the same natural-size viewer and
GIF Play/Pause control. Chat previews themselves remain static.
Loading previews are rectangles sized from image metadata, including EXIF rotation,
before pixel decoding starts. The rectangle, loaded preview and error/retry state
share the same aspect-ratio layout, so decoding completion does not move the chat.
Metadata is probed during source loading where the format permits it. A bounded
in-memory cache remembers the last 256 resolved sources' dimensions across scrolling
and session switches. On the first load
(or after that cache is evicted/restarted), a neutral rectangle can resize once the
previously unknown dimensions arrive. Changed source dimensions can also resize it.
Loading and error states preserve the alt text. Click a failed image rectangle to
retry; hover it for the full error details (even for very thin images).
Ordinary links to image files remain links, not embedded previews.

Markdown images are **not saved with the conversation** and are not added to model
context as media. Only their Markdown source text is persisted. When a message leaves the mounted transcript, pending loads are
cancelled. Successfully loaded previews can be reused immediately on scroll-back,
including their original bytes for the viewer. This offscreen cache is shared with
decoded tool/attachment thumbnails, retains up to 64 MiB and 256 entries, and
evicts the least recently viewed entries first. Failures are not cached offscreen.
Switching sessions clears offscreen Markdown previews, not saved-media thumbnails.
Until eviction a preview can show the previously loaded image even if its source
changes or disappears; after eviction it reloads the original. An already open
modal can finish using its loaded bytes. Two source loads and two shared decoders may run concurrently;
the retained Markdown cache is capped at 256 MiB across visible messages. If that
limit is reached, use fewer/smaller images or scroll other image messages out of view
and retry. Images never create files in session media storage.

## Reference provenance and live acceptance

Public OpenAI Codex source, Apache-2.0, checked at commit
`b1573f102413e96a6481087922bf425231222dee`:

- `ext/image-generation/src/tool.rs`, `imagegen_description.md`, `imagegen_gif_description.md`
- `core/src/tools/handlers/view_image.rs`, `view_image_spec.rs`, `core/src/image_preparation.rs`
- `code-mode-runtime/src/runtime/{content.js,value.rs,audio.rs}`
- `protocol/src/models/mcp_content.rs`, `codex-api/src/images.rs`

Repository: <https://github.com/openai/codex>. Orbit independently implements the
contracts in Rust/QuickJS, not Codex/V8 or its extension/attachment machinery.
Image preparation shares its budget and validation approach, not byte-identical
encoding. Metadata is not retained in prepared PNGs; originals remain unchanged.

API references: [generate](https://developers.openai.com/api/reference/resources/images/methods/generate)
and [edit](https://developers.openai.com/api/reference/resources/images/methods/edit).

Automated tests use local fixtures, never user credentials. The following live
acceptance checks consume account usage and must be authorized separately:

1. Codex sign-in: generate a PNG, edit it with an absolute reference, inspect both.
2. API key + explicit API selection: generate/edit with the configured model;
   verify separate billing and requested model/quality in the result hint.
3. Generate a 2-frame GIF if the account has access; verify animation and pause in
   the modal. If unavailable, confirm an explicit backend error with no fallback.
4. Try an invalid format, reference combination and existing destination: confirm
   actionable errors and no remote request.
5. Restart; move/delete the caller image. Open the saved tool thumbnail/modal.
6. Switch sessions and inspect a child image. Confirm children cannot call imagegen.
7. Cancel a slow generation: confirm the uncertainty/recovery message; inspect the
   destination and recovery artifacts before choosing whether to repeat anything.

Public Codex source and HTTP fixtures do not establish live subscription model
entitlement or availability. GIF is backend/account-dependent; unsupported quality
combinations are also expected service errors, not reasons for implicit fallback.
