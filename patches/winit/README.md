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

Modified upstream files: `Cargo.toml`, `src/platform/wayland.rs`, Wayland `mod.rs`,
`state.rs`, `seat/mod.rs`, `seat/keyboard/mod.rs`. Added `data_device.rs` and `data_device/uri_list.rs`.
`url` decodes spaces/Unicode and rejects nonlocal file URIs. The app test suite
includes this exact URI parser, including its regressions; `just verify` runs it.
The release archive's `Cargo.lock` is not used: the application root lockfile is
canonical. Remove this patch when the pinned GUI stack provides these features
upstream; never edit Cargo's registry source cache.


## How builds use it

- Nix fetches the checksum-pinned archive and applies the patch with `--fuzz=0`.
  A download/hash mismatch or rejected patch fails preparation.
- `nix develop` creates/refreshes the Git-ignored `.build-deps/winit` symlink to
  that immutable patched source. Enter the shell from this checkout (the root
  or a subdirectory); `nix develop --command cargo …` works too. A missing link
  after a fresh clone is expected until the first `nix develop`.
- The root `[patch.crates-io]` redirects winit to this generated path. Pico's
  direct dependency is exactly `=0.30.13`; `Cargo.lock` pins the resolved graph.
- Package and formatting checks create the same link in a separate prepared
  source tree. They never depend on an existing working-tree link or network
  access during Cargo compilation.
- Cargo's registry cache is never modified. There is no remote fork, Cargo
  wrapper, or change to compiler flags, and `target/` remains reusable.
- The URI parser regression is compiled from the patched source by the app's
  tests (`include!`), not duplicated in the project. Formatting the app does
  not try to rewrite the read-only dependency in the Nix store.

Re-enter `nix develop` after editing the patch or pin. Do not edit `.build-deps`:
its source is immutable and the link is managed automatically.

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
