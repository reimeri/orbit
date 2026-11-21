# Iced widget patches

Base: crates.io `iced_widget` **0.14.2**. See [development](../../docs/development.md#environment)
for how Nix prepares the patched sources.

The editor-access.patch adds Content::with_editor, a read-only callback exposing the
underlying renderer editor. Orbit's composer needs its actual shaped buffer and
scroll offset: logical line counts or accumulated wheel deltas cannot represent
wrapped lines or cursor-driven scrolling correctly.

The editor-performance.patch adds mutable renderer access for absolute native
scroll requests, applied in application-message order with edits and wheel actions.
`growing_editor.rs` reads already-cached native line layouts, estimates unmeasured
offscreen heights, and sends logical-line/fraction seeks for track/drag actions.
It never shapes offscreen text just to draw the scrollbar, and seeking does not
move the caret or selection. Wheel input and keyboard reveal remain native.

pick-list-f32.patch removes the obsolete std::f32 import so f32::INFINITY
resolves to the primitive type’s associated constant instead of the deprecated
module constant. This does not change pick-list behavior.

menu-gutter.patch reserves a permanent 7 px right gutter (a 5 px
scrollbar hit area plus a 2 px gap) in built-in pick-list menus. These menus
construct their own Scrollable and do not expose content layout to the app.
The patch also sets their rail to 5 px and thumb to 3 px, matching the app.
The padded row keeps option text, backgrounds and hit targets out of the rail
without adding a container-theme bound. Menu text is clipped to the list bounds
so long labels cannot paint into that gutter. Keep this width in sync with Orbit's
scrollbar::GUTTER. Like the app's scroll wrapper, it deliberately does not use
Scrollable::spacing, which changes the available width when overflow changes.

menu-click-through.patch lets outside presses close a built-in pick-list menu
without capturing the event, so the same click also activates the underlying
control. Presses on the selector itself remain captured to close without reopening.
The app's context menus and model/reasoning selector use the same policy through
`pointer_input::dismiss_layer`; modal dialogs deliberately remain blocking.
The `settings_dropdown_outside_click_activates_controls` regression exercises
controls on both sides of the selector in widget traversal order.

markdown-table-wrap.patch gives Markdown table columns a share of the actual
viewport width. The table can shrink around short cells; when the viewport is
too narrow, its layout budget stops shrinking at eight ems of text per column
plus padding, and the existing horizontal scrollbar handles any overflow. The
width constraint is opt-in: code blocks and other scrollers remain unbounded.
Keeping it in the native Markdown widget (which owns the private row cells)
preserves the same viewer traversal for rendering, selection and copy.

The existing input-method fallback anchors empty paint geometry at the text
viewport origin while keeping the focused editor enabled. This lets IME commits
reveal an offscreen caret without requiring offscreen measurement.

display-math.patch carries a block equation as one source-preserving item
through incremental parsing, rendering and copy. Its parser option comes from
the [pulldown-cmark block extension](../pulldown-cmark/README.md).

When updating iced, check which patches have upstream equivalents, regenerate
the remaining diffs, and update the pin and root lockfile. Re-enter the dev
shell and run `just verify`, `nix flake check` and `just smoke-gpu`.
