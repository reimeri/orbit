# Pico V2

A fresh Rust + [iced](https://iced.rs/) implementation of the Pico harness.
**Phase 0 is ready:** reproducible tooling, a native Wayland window, and a fast
incremental development loop. The actual harness starts in Phase 1; see
[PROJECT.md](PROJECT.md).

## Start developing

Target: **x86_64 Linux / NixOS, native Wayland only**. Enable Nix flakes and enter
an existing Wayland desktop session. No system Rust installation is needed.

```sh
nix develop
just run
```

The smoke window has a text field, an interaction counter, and a reset button.
It uses wgpu with a tiny-skia software fallback, has application ID `pico-evo`,
and does not read or write old Pico configuration or sessions.

```sh
# Inside nix develop:
just check       # Fast type checking; no executable linking
just test-core   # Core tests without the GUI dependency graph
just verify      # Format, Clippy, workspace tests, headless window smoke check
just smoke-gpu   # Force wgpu on your actual Wayland desktop

# Reproducible release package (not the everyday edit/build loop):
nix build
nix run
nix flake check  # Package + all unit tests + headless window + formatting
```

The Nix package includes runtime library paths, a fallback font configuration,
and a desktop entry. The host supplies GPU drivers. The software window check
runs on a private headless Weston compositor; it does not use your desktop or
require a GPU. `just smoke-gpu` briefly opens and closes a desktop window.

## Layout

- `crates/pico-app`: `pico-evo` executable, iced UI, window lifecycle.
- `crates/pico-core`: UI-independent foundations; currently the isolated V2
  configuration-path policy. Keep iced out of this crate.
- `scripts/smoke-wayland.sh`: bounded, isolated window smoke check.
- `flake.nix` / `flake.lock`: pinned toolchain, libraries, dev shell, package/checks.
- `Cargo.lock`: pinned Rust dependencies.
- `docs/development.md`: build-performance guidance, verification, troubleshooting.
- `pico-reference/`: local, ignored V1 reference only; not a dependency or build input.

For Phase 1, replace the smoke UI with the real application and add domain logic
and tests to the core. No empty agent/tool/plugin abstractions have been added.
