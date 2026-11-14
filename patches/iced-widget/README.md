# Iced editor scroll metrics access

Base: checksum-pinned crates.io iced_widget **0.14.2**. The Nix development
shell, package, and formatting check prepare .build-deps/iced-widget using
default.nix; Cargo redirects this dependency to that immutable patched source.
No registry cache or upstream source is modified in the checkout.

The patch adds only Content::with_editor, a read-only callback exposing the
underlying renderer editor. Pico's composer needs its actual shaped buffer and
scroll offset: logical line counts or accumulated wheel deltas cannot represent
wrapped lines or cursor-driven scrolling correctly.

growing_editor.rs uses the buffer's native metrics and layout parameters,
caches offscreen line layouts, and sends ordinary Action::Scroll messages for
thumb dragging and track clicks. It does not change the editor's text, selection,
keyboard handling, or scroll model.

When updating iced, regenerate the patch against the official release, update
the version/hash and root lockfile, and re-enter nix develop. Never edit the
generated symlink or registry source. Remove the patch when upstream provides
equivalent read access. Run just verify, nix flake check, and just smoke-gpu
from the Nix environment after patch changes.
