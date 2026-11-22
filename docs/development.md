# Development and compilation

## Environment

Use `nix develop` for Rust, Cargo, rust-analyzer and native libraries. Launch
your editor from the shell or configure it to use the flake. The supported
toolchain is pinned by `flake.lock`. Keep iced out of `orbit-core`; the app uses
wgpu on native Wayland, without an X11 backend or software-renderer fallback.

The flake has a source allowlist. New build inputs must be known to Git
(`git add <files>`) as well as included by the allowlist. The shell prepares
`.build-deps/` links to checksum-pinned patched dependencies in the Nix store;
package and formatting checks prepare their own links. Do not edit the links or
Cargo's registry. Re-enter the shell after changing a patch. Nix normalizes
mtimes: if Cargo reuses a stale artifact, clean only the affected crate and
profile. See the patch notes for the pin, rationale and regression checks:

- [notification activation: iced runtime, iced-winit and winit](notifications.md#native-window-activation)
- [winit: clipboard and file drops](../patches/winit/README.md)
- [iced-winit: screenshot ordering](../patches/iced-winit/README.md)
- [iced-wgpu: image, quad and scissor snapping](../patches/iced-wgpu/README.md)
- [iced-widget: editor, menus and Markdown](../patches/iced-widget/README.md)
  and [pulldown-cmark: block math](../patches/pulldown-cmark/README.md)
- [cosmic-text: shaping](../patches/cosmic-text/README.md) and
  [iced-graphics: caret and selection](../patches/iced-graphics/README.md)
- [QuickJS: pending-module lifetime](../patches/quickjs/README.md)

## Choose the cheapest useful check

Run these from the repository root inside `nix develop`:

| Situation                       | Command                                            |
| ------------------------------- | -------------------------------------------------- |
| Most edits                      | `cargo check --locked -p orbit` or `just check` |
| Core logic                      | `cargo test --locked -p orbit-core`                 |
| One test                        | `cargo test --locked -p orbit-core <test-name>`     |
| Need to see the UI              | `just run`                                         |
| Renderer/Wayland changes        | `just smoke` and `just smoke-gpu`                  |
| Before handing off a change     | `just verify`                                      |
| Packaging/toolchain changes     | `nix flake check` and `nix run`                    |
| Actual performance measurements | `cargo run --locked --release`                     |

`just test` selects the pinned Mesa Lavapipe CPU Vulkan driver. For a focused UI
test use `just test <test-name>` or
`./scripts/test-renderer.sh cargo test --locked -p orbit <test-name>`.
`just verify` checks formatting, Clippy, workspace tests, QuickJS under
Valgrind, and rendered frames on a private headless Weston compositor.
Tests use temporary state and local fixtures; they do not verify login or live
model access. The package smoke test uses the wrapped installed binary.
`just smoke-gpu` checks your desktop driver, which Lavapipe cannot validate.

Keep `target/` and the same toolchain, linker (mold), profile and compiler flags
between builds. Dev/test profiles are incremental and retain line tables, not
full dependency debug info. Nix packaging builds do not reuse local artifacts.
Use `just timings` for a Cargo timing report, a temporary `CARGO_TARGET_DIR`
for cold measurements, or `CARGO_BUILD_JOBS=4` if memory is constrained.

## Capture a marketing screenshot

From the repository root, run:

```sh
nix develop --command just marketing-shot
```

This writes **`target/orbit-marketing.png`**, a 2560 × 1600 capture of the
window contents (1280 × 800 at 2× scale). To choose a different output file,
pass its path, for example `just marketing-shot "/tmp/Orbit demo.png"`
inside the dev shell. The parent directory must exist.

The recipe uses the smoke test’s private headless Weston display and pinned
CPU Vulkan driver. It doesn’t need a desktop or login, use your Orbit state,
or execute the commands shown in the chat. Edit the [staged sessions and
transcript](../crates/orbit-app/src/ui/marketing.rs) to change the scene.

To rebuild the banner at the top of the README from that capture, run:

```sh
nix develop --command just marketing-banner
```

This combines the full window with the title and background in
[`assets/marketing/`](../assets/marketing/) and writes `orbit-banner.png` there.
It does not retake the screenshot. To preview an alternate capture without
changing the README image, use `just marketing-banner "/tmp/Orbit demo.png" "/tmp/banner.png"`
inside the dev shell.

## Runtime troubleshooting

- **No Wayland session:** verify `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` from a
  terminal in your Wayland desktop. Xwayland is intentionally unsupported.
  For SSH/CI use `just smoke` instead of inventing a display value.
- **GPU startup trouble:** verify the host's Vulkan/graphics driver installation
  and Wayland session, then retry. On NixOS keep `hardware.graphics.enable` and
  the appropriate vendor driver enabled. Re-enter `nix develop` if loader libraries
  are missing. There is no application software fallback. Remove stale
  `ICED_BACKEND`, `WGPU_BACKEND`, `VK_DRIVER_FILES` or `VK_ICD_FILENAMES` overrides
  when testing normal startup; the installed wrapper supplies loaders, not a
  machine-specific graphics driver.
- **No Vulkan adapter for tests:** run `just test` inside `nix develop` to select
  the pinned CPU driver, or use `scripts/test-renderer.sh` for a focused Cargo
  command. Do not export its ICD selection in your normal desktop environment.
- **Missing native library or linker:** re-enter `nix develop`. A raw
  `target/debug/orbit` is intended to run inside the dev environment. Use the
  wrapped `nix run` / `result/bin/orbit` outside it.
- **Fonts:** the shell and package provide a fontconfig fallback containing
  DejaVu and normal system/user font directories. The package preserves an
  explicitly supplied `FONTCONFIG_FILE`.
- **Unexpected rebuild:** check changes to the compiler, features, profile,
  `RUSTFLAGS`, target triple, linker configuration and environment before cleaning.
  Reopening the same pinned shell alone should not require recompilation.
- **Smoke timeout:** rerun `just smoke` and inspect its printed app/Weston logs.
  Test `just smoke-gpu` separately to distinguish CPU-driver/window plumbing from
  the host GPU stack. A Lavapipe check passing does not certify the host GPU.

## Implementation notes

`crates/orbit-core` owns persistence, OAuth, the provider, agent loop and tools.
`crates/orbit-app` owns iced UI, clipboard, browser launch and folder selection.
The installed wrapper provides Git and xdg-open; the desktop provides the portal
for the optional folder chooser. Manual path entry needs no portal.

The Wayland app ID, desktop entry and icon name are all `orbit`. Packaging resizes
[`assets/icon/orbit.png`](../assets/icon/orbit.png) for the desktop icon theme. The square sRGB master keeps
its transparent corners; the package downsizes it without cropping. Test the
optional offline migration with `python3 -m unittest discover -s tests`.

- [Code mode](code-mode.md) documents execution, resource limits and recovery.
  [Responses transport](responses-websocket.md) covers connection reuse and fallback
  to full input.
- [Session and rendering performance](performance.md) covers storage boundaries,
  recovery, and viewport caches. For measurements, see the
  [whole-app performance audit (HTML)](benchmarks/app-performance.html),
  [session and rendering benchmarks](benchmarks/session-and-rendering.md),
  [editor benchmarks](editor-performance.md) and [file-index benchmarks](file-tags.md#verification-and-benchmarks).
- Use `theme::button` and the shared styles in `crates/orbit-app/src/theme.rs`
  rather than hard-coded colors. For vertical scrolls use `scrollbar::scroll`:
  it reserves the rail even when content fits. Keep row backgrounds and hit
  targets outside that gutter. Use `scrollbar::horizontal` for horizontal scrolls.
- Test captures share one Vulkan engine. Serialize prepare/render/readback in
  `test_renderer.rs` because cloned iced MSAA pipelines share resizable targets.
  CPU fixture preparation can run in parallel.
- To update the fixed model catalog, follow the
  [model-update skill](../.agents/skills/update-openai-models/SKILL.md).
