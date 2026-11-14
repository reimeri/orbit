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
wgpu, tiny-skia, Wayland, the Tokio executor, and crisp rendering. Neither
winit nor softbuffer enables X11. Do not reintroduce default features accidentally.
Phase 1 uses Tokio for provider streams, OAuth and UI tasks.

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
actually opening the UI. Core-only tests compile networking, storage and QuickJS dependencies but do not compile iced. `just verify` runs format checking, Clippy
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
out after 60 seconds, reports logs on failure, and removes its private runtime
directory and compositor on exit. It does not change your existing compositor.

`just smoke-gpu` forces `ICED_BACKEND=wgpu`, disables `DISPLAY`, and exercises
your current Wayland desktop and graphics driver without software fallback.
The same headless software smoke check runs against the **wrapped installed
binary** in the Nix package's sandboxed install-check phase. GPU checks remain
manual: sandbox/CI machines do not necessarily have a GPU.

The initial checks passed for both renderers, and the desktop window was
visually inspected. GUI state transitions have a unit test, but the smoke check
is **not** an automated click/keyboard/layout test. For manual verification, run `just run` and follow
[the Phase 1 acceptance checklist](phase-1-testing.md). Smoke mode uses private
temporary application state, no credentials and no provider calls. Its rendered views exercise the project picker, Markdown/tool details, settings,
modal and session popover, plus scrolling a ~1 MiB tool result to the middle/end
and reflowing it with Settings open. Normal mode persists configuration and sessions under `pico-evo`.
Inspect feature selection after dependency changes with:

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

## Phase 1 architecture

`pico-core` now owns isolated atomic storage, subscription OAuth/refresh, Responses
streaming, the agent state machine, command sessions and embedded QuickJS cells.
`pico-app` owns iced widgets, Markdown presentation, the folder chooser, clipboard
and browser-launch actions. There is no iced dependency in the core and no V1
state migration or extension support. See [README](../README.md) for runtime state
paths and [code mode](code-mode.md) for the tool contract.

Tests use temporary storage and local HTTP fixtures. They cover token rotation,
state validation, storage failures, fragmented/truncated streams, provider/tool
roundtrips, bounded compaction, shell/PTYS, cancelled cells and UI modal/draft
regressions. Live ChatGPT access remains a user-run acceptance test. Packaged
runtime PATH includes Git and xdg-open; the desktop supplies its portal backend
for the optional folder chooser (manual path entry always works).

## UI performance follow-up

The UI uses coalesced core change notifications and per-conversation revisions,
not a permanent 50 ms session-snapshot timer. Only changed conversations are
cloned. Activity animation runs while a foreground turn is active; draft saving,
Git refresh and session age labels have independent low-frequency timers.
Redraw events are not fed back into the update loop. Idle pointer motion is not
published as application messages either: a root widget resolves click positions
in window coordinates and forwards motion only during an active panel resize.
This preserves context-menu placement in scrolled sidebars and avoids view/layout
rebuilds just to remember the cursor. Normal widget hover handling remains intact.
Sidebar session rows use widget-local 180 ms cubic ease-out Y transitions when
filtering or reordering changes their layout. Identity survives catalog/search
updates; measurement epochs remain separate. Only rows mounted before and after
the change animate, so newly visible/removed rows snap and virtualization stays
bounded. Redraws advance the layout (including hit targets) only while a transition
is active, without application messages or an idle animation timer.
Large tool details use a
fixed-row virtual viewport, including long single-line output and Unicode; only
about 22 rows are passed to text shaping. Collapsing a tool or leaving its session
releases its detail cache. The complete output remains available through Copy.

Chat selection is stored as logical entry/block/UTF-8 positions outside the
virtualized widget tree. A custom Markdown viewer preserves iced's layout and
styles while selectable text uses the renderer's shaped paragraphs for hits and
highlights. Ctrl+C reconstructs only the selected entries, including unmounted
tool rows; it does not depend on the visible widget slice. Selection is invalidated
when selected entries change or the session/content visibility changes. Pointer
motion and scroll-induced redraws update selection locally, without idle app
messages. Interaction tests cover cross-message dragging, hard breaks/Unicode,
links, stationary-pointer wheel scrolling and streaming Markdown reparses.

A local before/after check using the same debug build profile, wgpu on the Wayland
desktop, isolated state and a two-message idle session measured **2.00% → 0.13% of
one CPU** over 15 seconds after a three-second warm-up. This is one local sample,
not a cross-machine performance guarantee; it includes periodic Git refreshes.

## Long sessions and large libraries

Session persistence now uses indexed SQLite transactions and structurally shared
histories. UI storage jobs are serialized off-thread; sidebar summaries load
without transcript bodies. Both transcript and sidebar use variable-height
viewport indexes, and visible streaming Markdown is parsed incrementally.
See [performance](performance.md) for architecture, format-change implications,
regression coverage, measurement limitations and the isolated release benchmark.
The window smoke check also exercises 20,000-entry and 10,000-session views.

## Subagents

`pico-core/src/agent/subagents.rs` owns fixed profiles, typed assignment states,
parent-scoped scheduling and event-driven waiting. The harness coordinates spawn,
Stop, family deletion and completion delivery without holding locks across awaits.
Child/session acceptance is transactional in `store.rs`; terminal results are
separate durable SQLite records, and unsaved terminal data is retained for retry.

The existing conversation loop runs both parents and children. Completion notices
use the same durable model-boundary pipeline as steering, with idempotent notice
identities and explicit suppression after Stop. `code_mode.rs` supplies an optional
weak-harness delegation dispatcher only to main conversations. No iced types enter
the core, and there is no extension/tool plug-in registry.

Local HTTP fixtures gate individual requests to exercise true parallelism, failure,
continuation and wake-up/steering races without OpenAI credentials. Run
`cargo test --locked -p pico-core agent::subagents` for focused iteration. The UI
regression `child_navigation_preserves_parent_draft_scroll_and_disables_child_edits`
covers navigation, family deletion, disabled child controls and narrow layouts.
See [subagents](subagents.md) for user-facing behavior and live acceptance checks.

## Questionnaires

`pico-core/src/questionnaire.rs` validates requests and owns the per-conversation,
event-driven input queue. Code cells supply their cancellation tokens and persist
nested tool events through the normal observer. `pico-app/src/questionnaire.rs`
owns UI drafts, focus-aware keyboard routing, Unicode word deletion and the panel;
no iced types enter the core. Pending state is in-memory only, and restart explicitly
cancels unfinished asks. Run `cargo test --locked --workspace questionnaire` for
focused coverage. Window smoke stages exercise questionnaire states and constrained
layouts. See [questionnaires](questionnaires.md) for acceptance checks.

## Media pipeline

`media.rs` owns typed output parts, private immutable snapshots, bounded image/GIF
preparation and provider projection. `image_tools.rs` owns strict tool arguments,
Codex/API transport, billing settings and atomic no-replace output publication.
Code-mode buffers text and media in order; all text buffers are prepared before
consuming an output batch, so a spill failure remains retryable. Only the provider
boundary materializes data URLs for persisted media references.

`image_view.rs` in the app decodes thumbnails/modal frames on bounded workers,
rejects stale completion messages, and animates only an open playing modal.
Iced uses `image-without-codecs`; the core enables PNG/JPEG/WebP/GIF explicitly,
without image's broad default codec dependency set or any X11 backend.

Run `cargo test --locked --workspace` for media, generation fixture and UI lifecycle
regressions. The software/GPU window smoke includes the Images settings tab, static
thumbnail, animated modal, pause and natural-size static views using deterministic local
fixtures. See [images](images.md) for resource limits and paid live acceptance.

The expanded rendered smoke sequence now includes 39 frames (including media and
Markdown image previews, missing-source errors and the byte-backed GIF modal).
Its timeout is 60 seconds; a measured software debug run took ~33 seconds locally.

The Nix package supplies Git through `nativeCheckInputs` for Git fixture tests;
this is separate from the installed wrapper's runtime PATH.
