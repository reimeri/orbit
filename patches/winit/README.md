# Pico Wayland image input

Base: the official crates.io `winit` **0.30.13** release archive.
`default.nix` pins its version and SHA-256; `wayland-image-input.patch` contains
only Pico's changes. Upstream source and its license are fetched into the Nix
store, not copied into this repository.

Upstream 0.30 has no Wayland `wl_data_device` destination; iced's clipboard API
only exposes text. Pico's local patch registers an ordinary data device per seat,
negotiates Copy for local `text/uri-list` drops, and emits the existing
`WindowEvent::DroppedFile`. A small `platform::wayland::request_image_clipboard`
API lets Pico's worker request PNG/JPEG/WebP/GIF bytes from the focused client's
selection, without unsafe/raw handles or clipboard-manager protocols. It neither
owns/replaces the clipboard nor moves/deletes source files. No X11 features are
enabled.

All pipe reads run through calloop with nonblocking descriptors, a ten-second
timeout and byte limits (1 MiB of URI metadata; caller-provided image limit).
Dropped offers are owned by their transfer until finish/destruction, not the next
drag's enter/leave lifecycle. The implementation dispatches the small destination
protocol directly to avoid SCTK 0.19's destruction of still-transferring dropped
offers when a new drag enters. Clipboard requests arrive via a waking channel;
there is no polling timer. Selection follows the seat delivering the latest key
press and is cleared on keyboard-focus loss/removal. Pipe descriptors are created
close-on-exec so agent subprocesses cannot inherit clipboard transfers. The app waits for replies only off its UI thread.

Cargo uses the application's root lockfile, not the release archive's. App tests
include the patched URI parser directly so spaces, Unicode and rejection of
nonlocal URIs are checked against the production code. A missing
`.build-deps/winit` link on a fresh checkout is expected until `nix develop`.

## Updating the pin or patch

1. Download/unpack the official release in a temporary working directory. Verify
   its SHA-256 against the crates.io release checksum. Keep a pristine copy.
2. Apply `wayland-image-input.patch` with `patch -p1 --fuzz=0`, make the desired
   edits in that temporary source tree, and regenerate a unified diff against
   the pristine release. Include newly added files with `/dev/null` as their old
   path; exclude generated files, registry metadata and the upstream lockfile.
3. When upgrading, update `version` and `hash` in `default.nix`, the exact app
   dependency in `crates/pico-app/Cargo.toml`, and the root lockfile as needed.
   Never use a floating branch or relax the checksum to make a build pass.
4. Run `nix develop --command just verify`, `nix flake check`, and
   `nix develop --command just smoke-gpu` on a Wayland desktop. Confirm
   `cargo tree --locked -e features -i winit` still has no X11 backend.
   Follow `docs/images.md` for native input acceptance after behavior changes.
