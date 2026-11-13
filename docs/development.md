# Development and compilation

## One pinned environment

Use `nix develop` for **all development tooling**, including editor servers:
launch your editor from that shell, or configure its environment integration to
use the flake. The shell provides Rust/Cargo, rustfmt, Clippy, rust-analyzer and
Rust sources, mold, pkg-config, just, timing tools, and headless Wayland tools.
Rust is pinned by `flake.lock`; there is deliberately no second rustup toolchain
or floating rust-overlay. The initial lock supplies Rust 1.99.1 / Cargo 1.99.0;
iced 0.14 requires Rust 1.88 or newer. The supported/tested compiler is the Nix
pin, not every compiler above that lower bound.

Wayland, libxkbcommon, Vulkan loader, GL/EGL, fontconfig and DejaVu fonts are
supplied by Nix. Iced defaults are disabled: explicitly enabled features are
wgpu, tiny-skia, Wayland, the thread-pool executor, and crisp rendering. Neither
winit nor softbuffer enables X11. Do not reintroduce default features accidentally.
A later phase can replace the executor with Tokio if its actual runtime needs it.

New files used by a Git-backed flake must be known to Git (`git add <files>`);
otherwise Nix cannot see them. A dirty-worktree warning is normal during edits.
Do not copy `target/`, the V1 reference, or `.pico` session assets into Nix sources.
The flake uses an explicit source allowlist.

## Choose the cheapest useful check

Run these from the repository root inside `nix develop`:

| Situation | Command |
| --- | --- |
| Most edits | `cargo check --locked -p pico-evo` or `just check` |
| Core logic | `cargo test --locked -p pico-core` |
| One test | `cargo test --locked -p pico-core <test-name>` |
| Need to see the UI | `just run` |
| Renderer/Wayland changes | `just smoke` and `just smoke-gpu` |
| Before handing off a change | `just verify` |
| Packaging/toolchain changes | `nix flake check` and `nix run` |
| Actual performance measurements | `cargo run --locked --release` |

`cargo check` skips code generation and linking. It does not replace tests or
actually opening the UI. The core crate has no third-party dependencies, so
core-only tests do not compile iced. `just verify` runs format checking, Clippy
with warnings denied, all workspace tests, and the software window check.

## What keeps builds fast

- **Keep `target/`. Never routinely run `cargo clean`.** Cargo's fingerprinting
  and incremental compiler are the primary caches. Cleaning should be a last
  resort for demonstrated corrupt artifacts, not a first troubleshooting step.
- **mold is the stable linker choice**, configured only for the supported Linux
  target in `.cargo/config.toml` and supplied by the flake. Don't toggle linkers
  or `RUSTFLAGS` between ordinary commands: that invalidates dependencies.
- Dev and test profiles retain **line-table debug information** and incremental
  compilation, without full dependency debug info. Backtraces retain file/line
  locations; full local-variable debugging requires a profile change and rebuild.
- Leave dev optimization at Cargo's default (0). Do not globally optimize all
  dependencies at level 3 merely to make a trivial window run. Measure first;
  targeted dependency overrides can be added for a demonstrated runtime bottleneck.
- Use stable features and the same target/profile for everyday work. Dev/test
  share `target/debug`, but test harnesses and check metadata still need some
  separate artifacts. Release and explicit `--target` builds have separate output
  graphs. The first check after a full build can still perform significant work.
- **Nix builds are packaging checks, not an incremental edit loop.** They build
  an isolated release output, do not reuse local `target/`, and changing packaged
  source triggers another package build. Downloads/vendor dependencies remain
  cached. No LTO or single-codegen-unit release settings are imposed.
- Keep the Rust toolchain and lockfiles stable during feature work. Update them
  intentionally (`nix flake update`, `cargo update`), review the changes, and run
  verification. A new compiler or changed flags can legitimately rebuild the graph.
- No sccache, nightly compiler, hot-reload framework, or dependency-artifact Nix
  framework is added yet. sccache can help clean/CI builds but does not cache
  incremental Rust compilations; it is not a substitute for preserving `target/`.
  Add complexity only after timing shows a need.
- Cargo uses available cores by default. If memory is constrained, use
  `CARGO_BUILD_JOBS=4` (or `cargo build -j 4`) rather than disabling incremental
  compilation. There is no machine-specific CPU tuning or hard-coded job count.

### Initial measurements

Observed during Phase 0 on an AMD Ryzen 7 5800X3D (16 logical CPUs), 32 GiB RAM,
NixOS, pinned Rust 1.99.1; GPU + software renderers both compiled:

| Operation | Wall time |
| --- | ---: |
| First dev build, empty project `target/` | 35.15 s |
| Unchanged dev build | ~0.10 s |
| App-only edit, codegen and relink | 1.16 s |
| Warm workspace/all-targets check | 0.27 s |
| Core test build and run | 0.27 s |

These are individual observations, not performance guarantees or a before/after
benchmark. Downloads and Nix shell provisioning are excluded; crate sources
were already fetched. Do not clean just to reproduce the cold number.

```sh
time -p cargo check --locked --workspace --all-targets
cargo build --locked --timings
# Open target/cargo-timings/cargo-timing.html in your browser.
```

For a genuinely necessary clean-build experiment, use a separate temporary
`CARGO_TARGET_DIR`, preserving the normal cache. Remove only that temporary build
directory afterwards. Future large Nix/CI builds may benefit from cached Cargo
artifacts (e.g. Crane), but the local Cargo loop should remain unchanged.

## Window verification

`just smoke` builds the app, starts a private headless Weston compositor with a
software renderer, unsets `DISPLAY`, forces `ICED_BACKEND=tiny-skia` by default,
and launches `pico-evo --smoke-test`. The app requests an iced screenshot after
its window opens, checks nonzero size, the RGBA byte count and non-uniform pixels,
prints `SMOKE_OK`, and exits successfully only on a valid frame. This exercises
real window creation and rendering, not just process startup. The script times
out after 30 seconds, reports logs on failure, and removes its private runtime
directory and compositor on exit. It does not change your existing compositor.

`just smoke-gpu` forces `ICED_BACKEND=wgpu`, disables `DISPLAY`, and exercises
your current Wayland desktop and graphics driver without software fallback.
The same headless software smoke check runs against the **wrapped installed
binary** in the Nix package's sandboxed install-check phase. GPU checks remain
manual: sandbox/CI machines do not necessarily have a GPU.

The initial checks passed for both renderers, and the desktop window was
visually inspected. GUI state transitions have a unit test, but the smoke check
is **not** an automated click/keyboard/layout test. For manual verification,
run `just run`, type in the field, increment/reset the counter, resize the
window and close it normally. No application config/session files should be
created. Inspect feature selection after dependency changes with:

```sh
cargo tree --locked -e features -i winit
cargo tree --locked -e features -i softbuffer
```

## Runtime troubleshooting

- **No Wayland session:** verify `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` from a
  terminal in your Wayland desktop. Xwayland is intentionally unsupported.
  For SSH/CI use `just smoke` instead of inventing a display value.
- **GPU startup trouble:** retry `ICED_BACKEND=tiny-skia cargo run --locked`.
  Normal startup tries wgpu then software automatically; an explicitly forced
  backend opts out of fallback. Keep host NixOS graphics drivers enabled
  (`hardware.graphics.enable` and the appropriate vendor driver). The flake
  supplies loaders, not a conflicting bundled machine-specific driver/ICD.
- **Missing native library or linker:** re-enter `nix develop`. A raw
  `target/debug/pico-evo` is intended to run inside the dev environment. Use the
  wrapped `nix run` / `result/bin/pico-evo` outside it.
- **Fonts:** the shell and package provide a fontconfig fallback containing
  DejaVu and normal system/user font directories. The package preserves an
  explicitly supplied `FONTCONFIG_FILE`.
- **Unexpected rebuild:** check changes to the compiler, features, profile,
  `RUSTFLAGS`, target triple, linker configuration and environment before cleaning.
  Reopening the same pinned shell alone should not require recompilation.
- **Smoke timeout:** rerun `just smoke` and inspect its printed app/Weston logs.
  Test `just smoke-gpu` separately to distinguish software/window plumbing from
  the host GPU stack. A software check passing does not certify the host GPU.

## Phase 1 boundary

The executable is intentionally only a smoke window. `pico-core` currently
contains the tested XDG configuration-path policy using `pico-evo`, not a full
session format or persistence implementation. Introduce core domain modules as
Phase 1 needs them and keep tests independent of the GUI. When adding state,
cache or data directories, apply the same V2 isolation policy. No V1 migration,
extension loading, agent loop, shell execution or network access is implemented.
