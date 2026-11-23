# Cosmic-text editor performance patches

Base: checksum-pinned crates.io **cosmic-text 0.15.0**. Nix applies
`editor-performance.patch` with no fuzz and prepares `.build-deps/cosmic-text`.
Cargo uses this same source for development and packaging. Never edit the
symlink or Cargo registry cache.

The patch preserves the text and native advanced shaping pipeline:

- Multiline insertion collects the same backwards-built lines/attributes, reverses
  them once, and splices once instead of shifting a `Vec` for every inserted line.
- `Buffer::set_text_unshaped` allows iced to construct content without laying it
  out at temporary metrics/unbounded width. Ordinary `set_text` still shapes.
- `Editor::set_scroll` changes only the viewport and clears pending cursor reveal,
  so an absolute scrollbar seek does not move the selection or shape intervening
  lines. Wheel actions and keyboard cursor reveal remain native.
- Script/common fallback families accept their closest matching weight instead of
  rejecting every non-exact weight. This includes variable fonts whose advertised
  default weight differs from the requested weight. The existing weight-sorted
  candidates and shaping/cluster handling remain intact.
- The existing optional `shape-run-cache` is enabled by the iced graphics patch.
  Its key now includes RTL direction; mutating the font database invalidates the
  cache. Owned text/attributes/glyph payload is budgeted to 4 MiB, with at most
  4,096 entries to bound map overhead. Oversized runs are shaped normally without
  caching. On capacity pressure the cache is cleared; this is deliberately not a
  second editor/document model. `trim` maintains the payload accounting.

The app tests exercise actual pasted text, mixed line endings and selection
replacement; bounded initial/seek shaping; fallback preference with a deliberately
non-exact face weight; font removal invalidation; and cold-vs-cached bidi/cluster/
attribute layout. Existing ligature, selection, wheel, fractional viewport and
questionnaire tests run unchanged in scope. See `docs/editor-performance.md` for
benchmarks, including distinct-word input that cannot fit entirely in the cache.

For changes, regenerate the patch against the pinned official crate, re-enter
`nix develop`, and invalidate **only the affected dependency** artifacts if needed:
`cargo clean -p cosmic-text` for debug and `cargo clean --release -p cosmic-text`
for release. Nix normalizes source mtimes, so a changed generated symlink alone
can otherwise leave stale artifacts. Preserve the rest of `target/`.
Run `just verify`, `nix flake check`, and `just smoke-gpu` on a Wayland desktop.

## Incremental paragraph shaping

`incremental-shaping.patch` targets repeated shaping during edits to a long logical
line. `BufferLine` records the earliest unchanged byte prefix across `split_off`
and `append`; ordinary resets, new text/attributes, font invalidation and shaping
setting changes revoke reuse. This retains the existing glyph vectors, not a
second copy of the document or a larger global run cache.

Bidi resolution, Unicode line-break segmentation and wrapping still run in full.
A word can reuse its glyphs only when its logical byte range, blank status,
resolved span level and paragraph direction match the previous shape and it ends
strictly before the edit boundary. The boundary word is rebuilt; tabs are rebuilt
before the ordinary tab-stop pass so adjusted advances are never applied twice.
RTL glyph reversal is undone/restored around the shared paragraph build.

As with the original cosmic-text buffers, owners must reset shaping when changing
font systems/databases. Iced already resets lines on font-system revision changes;
this patch does not introduce a second font-generation mechanism.

The differential app tests compare complete edited layouts with fresh shaping,
including combining marks, mixed/changed bidi direction, isolates, tabs, repeated
edits before layout, same-range replacement, split/join attributes and explicit
invalidations. The benchmark now includes repeated tail insertion and backspace;
see `docs/editor-performance.md` for the diagnostic breakdown and remaining costs.

## Composer history replay

`replay-caret.patch` requests caret reveal on replay even if the final cursor
has the same coordinates; it also resets the remembered vertical-motion column.
`history-delete.patch` preserves separately authored CR and LF boundaries
when a deletion is reversed (re-parsing the flattened text could otherwise
coalesce lines, invalidating later byte offsets). It collects removed text only
while recording and drains removed lines together rather than repeatedly moving
the suffix when undoing a large paste. See the app's `editor_history` tests.

## Wrapped scrolling at the end of a draft

`wrapped-scroll-limit.patch` clamps the viewport at the end of the text even when
it starts inside the first wrapped paragraph. Upstream only corrects overscroll
when there is a preceding logical line. Scrolling farther down could therefore
expose blank space and make iced's shrink-to-fit editor measure fewer visible
rows. The clamp uses the row heights already measured for scrolling; it does not
shape offscreen text. The growing-editor wheel regression covers both an
overflowing draft and the same text reflowed to fit.
