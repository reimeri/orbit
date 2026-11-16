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
