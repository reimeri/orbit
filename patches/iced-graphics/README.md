# Iced graphics caret and selection patch

Base: checksum-pinned crates.io **iced_graphics 0.14.0**. Nix applies
`ligature-caret.patch` with no fuzz and prepares `.build-deps/iced-graphics` for
the development shell, package, and formatting check. Cargo uses the same patched
source everywhere; do not edit the generated link or Cargo's registry cache.

## Why this is needed

The upstream editor advances its logical cursor by grapheme, but positions the
visible caret by summing whole glyph widths. A font can shape multiple graphemes
into one glyph (`fi`, for example). The cursor between `f` and `i` consequently
appears after `i`, at the same position as the next arrow-key step. Selection
highlighting has the same whole-glyph assumption: copying a selection of only
`f` can correctly copy `f` while the highlight still covers all of `fi`.

The patch adapts cosmic-text 0.15's `cursor_glyph_opt` approach: distribute a
shaping cluster's advance across its graphemes, including clusters composed of
multiple glyphs, using their actual X positions and bidirectional level. Logical
row bounds use minimum/maximum byte indices because
RTL glyphs are not stored in logical order. Glyph-boundary affinity chooses the
appropriate side at directional transitions.

Calling cosmic-text's `Edit::cursor_position` directly is not equivalent: it
rounds coordinates to integers, searches only visible runs, and ignores affinity
at a boundary shared by wrapped rows. We retain iced's existing floating-point
vertical positioning, scroll offsets, wrapped-space handling and row affinity.
Caret and selection endpoints share the same cluster-to-X calculation. Selected
clusters are clipped to the logical range and adjacent visual spans are merged;
bidirectional selections can yield multiple disjoint spans on one row. Empty
and unselected wrapped rows still contribute to vertical offsets. Logical
selection, copying, and the existing mouse hit-testing remain unchanged.

This does not change keyboard movement, text contents, shaping, or fonts.
Proportional grapheme placement is an approximation,
like cosmic-text's implementation; it does not read font-specific ligature caret
tables.

## Maintenance and tests

The renderer tests in `growing_editor.rs` require DejaVu Sans (in the pinned
fontconfig). They check real ligatures, multi-glyph clusters, mixed-direction
boundaries, wrapping affinity and agreement between selection geometry and
copied text.

When updating iced, regenerate the patch against the official crate, update the
version/hash and Cargo lockfile, and re-enter `nix develop`. When revising an
existing patch, run `cargo clean -p iced_graphics` inside the shell once before
retesting: switching the generated symlink can leave Cargo artifacts stale
because Nix normalizes source mtimes. This invalidates only this dependency,
not the entire `target/` cache. Remove the patch when upstream provides
equivalent behavior. Run `just verify`, `nix flake check`, and
`just smoke-gpu` (on a Wayland desktop) after changes.

## Editor performance patch

`editor-performance.patch` enables cosmic-text's bounded/direction-aware shaping
cache (see `../cosmic-text/README.md`), defers shaping of initial text until native
layout, and sets metrics and viewport size together. Previously setting metrics
first caused a new editor to shape its entire document with unbounded dimensions.

`Editor::scroll_to` measures the destination logical line and seeks within its
height without moving the cursor. It is used by Orbit's estimated-height scrollbar;
ordinary wheel input and keyboard caret reveal still use native editor actions.

When checking release benchmarks after a patch change, use the matching targeted
invalidation, `cargo clean --release -p iced_graphics`, as well as re-entering
`nix develop`. Debug and release artifacts are separate.

An edit followed by a seek before the next layout can leave the caret's logical
line unshaped and offscreen. Selection geometry returns no paint regions for that
case instead of forcing layout or panicking; existing cached caret geometry is
preserved. A focused draw regression covers the edit/seek/reflow sequence.

## Composer spelling underline patch

`editor-range-spans.patch` exposes the existing caret calculation for a single
shaped cluster so Orbit can underline spelling ranges with the same cluster and
bidirectional geometry. The overlay reads visible native layout runs after
the editor draws; it does not change shaping, layout, scroll, or selection.

## Composer history

`editor-history.patch` exposes native change recording and replay through the
existing Content editor callbacks. Replaying invalidates the selection/highlight
caches and restores the exact native caret and selection (including affinity and
word/line mode). Grouping, memory bounds and draft ownership live in orbit-app.

`editor-indent.patch` implements the composer's four-space soft tabs using
native range edits, so block indentation preserves logical lines, selection
direction and undo history without replacing the whole draft. It does not
change how literal tabs are rendered.
