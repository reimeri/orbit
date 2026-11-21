# Iced screenshot ordering

Base: checksum-pinned crates.io **iced_winit 0.14.1**. Nix applies
`screenshot-ordering.patch` with no fuzz and prepares `.build-deps/iced-winit`
for development, packaging, and formatting checks. Cargo uses that same source
in all three environments. Never edit the generated symlink or registry cache.

## Failure and fix

Upstream executes `window::screenshot` against the renderer's last recorded
primitives, even after rebuilding the UI. Normal paragraph text is recorded via
weak references. Replacing the widgets or their paragraphs invalidates those
references: screenshots retain stale backgrounds/icons but omit text. A delay
before requesting a screenshot does not guarantee the next redraw's ordering.

Screenshot requests are now queued on their owning window. A private control
event runs the existing redraw handler's UI update/layout/draw and IME-preedit
steps before rendering the screenshot offscreen. Multiple pending requests for
the same window share the resulting image. No extra display-frame subscription
event is broadcast: otherwise a `window::frames` subscriber requesting captures
could recursively trigger itself.

Captures do **not** wait for presentation or configure/acquire a window surface.
Wayland frame callbacks can be withheld for occluded/minimized windows; waiting
for a successful presentation can stall captures indefinitely. The normal display
redraw path is unchanged. Nonzero-sized hidden windows can still be captured.
Closing a window, requesting a nonexistent window, or capturing at zero size
cancels the pending task by dropping its sender (the API has no error result).

## Regression coverage

`crates/orbit-app/src/screenshot_check.rs` runs at the beginning of the existing
native `--smoke-test`, before the usual app scenes. It replaces the view and
requests a capture in the **same update**, with no settling timer, then changes
the text and moves/recolors a marker while immediately requesting two captures.
Each image must contain text and the current marker, with the old marker position
cleared. Both queued responses must arrive and agree. This detects missing text,
stale content/layout, and lost queued responses through the real window API.

The fixture uses explicit geometry/colors rather than production theme/layout
snapshots. An offscreen validator test at integer and fractional scales also
checks that deliberately stale or text-less images fail; it does not substitute
for the native API check. The native regression runs in `just verify`,
`just smoke-gpu`, and the installed package check. Minimized-window and
surface-error lifecycle behavior is not separately simulated by these tests.

When changing this patch, re-enter `nix develop` and invalidate only the patched
crate once with `cargo clean -p iced_winit`: Nix-normalized source mtimes can
otherwise leave stale artifacts. Preserve the rest of `target/`. Run
`just verify`, `nix flake check`, and `just smoke-gpu` on a Wayland desktop.
Remove this patch when upstream provides equivalent capture ordering.
