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

The editor regression tests in `growing_editor.rs` exercise the real patched
renderer editor. They require DejaVu Sans (provided by the pinned Nix fontconfig),
assert that their fixtures actually form ligatures, and cover both arrow
directions, combining marks, RTL lam-alef (including a separate mark glyph),
mixed-direction boundaries,
wrapped-away spaces, wrapping affinity and scroll geometry. Selection regressions
check individual graphemes in both directions, actual click/drag and copied text,
combining marks in multi-glyph clusters, disjoint bidi spans, empty/wrapped rows,
and scrolling.
They run as part of normal workspace tests, including the packaged build.

When updating iced, regenerate the patch against the official crate, update the
version/hash and Cargo lockfile, and re-enter `nix develop`. When revising an
existing patch, run `cargo clean -p iced_graphics` inside the shell once before
retesting: switching the generated symlink can leave Cargo artifacts stale
because Nix normalizes source mtimes. This invalidates only this dependency,
not the entire `target/` cache. Remove the patch when upstream provides
equivalent behavior. Run `just verify`, `nix flake check`, and
`just smoke-gpu` (on a Wayland desktop) after changes.
