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
wgpu, Wayland, the Tokio executor, and crisp rendering. There is no alternate
application renderer, softbuffer dependency in the active graph, or X11 backend. Do not reintroduce default features accidentally.
Phase 1 uses Tokio for provider streams, OAuth and UI tasks.

New files used by a Git-backed flake must be known to Git (`git add <files>`);
otherwise Nix cannot see them. A dirty-worktree warning is normal during edits.
Do not copy `target/` or the V1 reference into Nix sources.
The flake uses an explicit source allowlist. Entering `nix develop` also prepares
`.build-deps/winit`, a Git-ignored link to the checksum-pinned upstream source with
our local patch applied in the Nix store. Nothing is downloaded or patched in
Cargo's registry cache. Re-enter the shell after changing `patches/winit/`.
Screenshot requests use a checksum-pinned iced window-shell patch to draw the
current UI before capture without waiting for compositor presentation; see
[the capture contract and regression check](../patches/iced-winit/README.md).
The shell prepares `.build-deps/iced-winit` for this patch too.
The composer also uses a checksum-pinned iced widget patch to read native editor
scroll metrics; see [its maintenance notes](../patches/iced-widget/README.md).
The shell prepares `.build-deps/iced-widget` alongside winit.
Editor paste, initialization, font fallback and bounded shaping-cache patches also
prepare `.build-deps/cosmic-text`; see [maintenance](../patches/cosmic-text/README.md)
and [editor benchmarks](editor-performance.md).
Caret positioning and selection highlighting inside ligatures use a
checksum-pinned iced graphics patch;
see [its maintenance notes](../patches/iced-graphics/README.md). The shell also
prepares `.build-deps/iced-graphics`, shared by development and packaged builds.
Code-mode's QuickJS engine and its Rust module-handle wrapper also use pinned
patches to fix pending top-level-await teardown; see
[`patches/quickjs/README.md`](../patches/quickjs/README.md). The shell prepares
`.build-deps/rquickjs-sys` and `.build-deps/rquickjs-core` and supplies Valgrind.
`just test-quickjs-memory` checks the lifetime regressions for invalid accesses
and leaked module references. It is included in `just verify` and the Nix
package's checks: successful tests alone cannot rule out a silent use-after-free.
The package and formatting checks prepare their own links, independent of the
working tree. See [patch maintenance](../patches/winit/README.md).

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
with warnings denied, all workspace tests, and the wgpu/Vulkan window check.
`just test` selects the pinned CPU Vulkan driver for rendering tests; focused tests
can use `./scripts/test-renderer.sh cargo test --locked -p pico-evo <test-name>`.
Plain `cargo test` also works when a Vulkan adapter is already available.

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
NixOS, pinned Rust 1.99.1, before the wgpu-only conversion. These historical
measurements included both application renderers:

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

`just smoke` builds the app and starts a private headless Weston compositor
(using pixman for the compositor itself). Pico renders through **wgpu/Vulkan**;
`scripts/test-renderer.sh` selects the pinned Mesa Lavapipe CPU Vulkan driver and
unsets the legacy Vulkan ICD override. No desktop or physical GPU is required.
The script unsets `DISPLAY`, launches `pico-evo --smoke-test`, and checks nonzero
screenshot size, the RGBA byte count, and non-uniform pixels. Before the app scenes,
a native screenshot-ordering fixture changes text and marker placement and
immediately requests captures. It requires visible text, current marker placement,
and both queued responses (`SCREENSHOT_CHECK_OK`), catching stale or text-less
frames that the nonblank check alone cannot detect. It prints `SMOKE_OK`
and exits successfully only on valid frames. The 60-second timeout reports logs
on failure; the private compositor/runtime directory is cleaned up afterward.

The development shell exposes `PICO_TEST_VULKAN_ICD` only as a path. It does not
set `VK_DRIVER_FILES` or `WGPU_BACKEND` globally. `just test`, `just smoke`, and the
Nix package's checks select Lavapipe only for their test processes. The installed
wrapper contains neither a CPU-driver selection nor an application fallback.

`just smoke-gpu` uses wgpu on your actual Wayland desktop and host graphics driver.
Keep this separate from CPU-driver tests: Lavapipe does not certify hardware GPU
behavior. The Nix package runs the same automated tests, then runs the headless window smoke
against its **wrapped installed binary**, without needing GPU device access.

The window smoke complements the offscreen wgpu pixel and widget tests, but is
**not** an automated click/keyboard/layout test. For manual verification, run `just run` and follow
[the Phase 1 acceptance checklist](phase-1-testing.md). Smoke mode uses private
temporary application state, no credentials and no provider calls. Its rendered views exercise the project picker, Markdown/tool details, settings,
modal and session popover, plus scrolling a ~1 MiB tool result to the middle/end
and reflowing it with Settings open. Normal mode persists configuration and sessions under `pico-evo`.
Inspect feature selection after dependency changes with:

```sh
cargo tree --locked -e features -i winit
cargo tree --locked -p pico-evo -e normal,build,dev  # No iced_tiny_skia or softbuffer
```

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
  `target/debug/pico-evo` is intended to run inside the dev environment. Use the
  wrapped `nix run` / `result/bin/pico-evo` outside it.
- **Fonts:** the shell and package provide a fontconfig fallback containing
  DejaVu and normal system/user font directories. The package preserves an
  explicitly supplied `FONTCONFIG_FILE`.
- **Unexpected rebuild:** check changes to the compiler, features, profile,
  `RUSTFLAGS`, target triple, linker configuration and environment before cleaning.
  Reopening the same pinned shell alone should not require recompilation.
- **Smoke timeout:** rerun `just smoke` and inspect its printed app/Weston logs.
  Test `just smoke-gpu` separately to distinguish CPU-driver/window plumbing from
  the host GPU stack. A Lavapipe check passing does not certify the host GPU.

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
The selected project refreshes Git every five seconds, both idle and running,
and immediately on selection (including startup restoration). Results are cached
per project for the lifetime of the app, so switching sessions does not blank
previously loaded status; in-flight captures are coalesced per project.
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
releases its detail cache. The complete output remains scrollable and supports selection and Ctrl+C.

Transcript and tool-row widget state now follows stable content identity, not
viewport slots or measurement epochs. The shared keyed column remaps actual child
trees for same-sized sliding windows and reorders; it retains only mounted rows.
See [performance](performance.md#text-layout-cache-reuse) for measured text-layout
costs and the cache-reuse regressions.

Chat selection is stored as logical entry/block/UTF-8 positions outside the
virtualized widget tree. A custom Markdown viewer preserves iced's layout and
styles while selectable text shares a single renderer paragraph for painting,
hits, links and highlights. Ctrl+C reconstructs only the selected entries, including unmounted
tool rows; it does not depend on the visible widget slice. Selection is invalidated
when selected entries change or the session/content visibility changes. Pointer
motion and scroll-induced redraws update selection locally, without idle app
messages. Interaction tests cover cross-message dragging, hard breaks/Unicode,
links, stationary-pointer wheel scrolling and streaming Markdown reparses.

Double-click selects a Unicode word (including across Markdown spans); triple-click
selects a Markdown paragraph, a blank-line-delimited plain-text paragraph, or a
logical code/tool-output line. Dragging after either gesture extends by whole
units in either direction. Click sequences use iced's timing/distance rules and
reset after drags, unrelated clicks or selection invalidation. Links still
activate immediately on release, including during multi-clicks; no delay is added.
Tool output shares a logical boundary index across visible rows, so wrapped words
and lines can be selected beyond the mounted viewport without shaping extra text.

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
regressions. The CPU-Vulkan/host-GPU window smoke includes the Images settings tab, static
thumbnail, animated modal, pause and natural-size static views using deterministic local
fixtures. See [images](images.md) for resource limits and paid live acceptance.

The expanded rendered smoke sequence now includes 47 frames (including media and
Markdown image previews, missing-source errors and the byte-backed GIF modal).
Its timeout is 60 seconds; timings from the former software backend are not a
performance baseline for wgpu/Lavapipe.

The Nix package supplies Git through `nativeCheckInputs` for Git fixture tests;
this is separate from the installed wrapper's runtime PATH.

## Standalone web access

`web_search.rs` owns Codex `alpha/search` authentication/transport and UI-only result
metadata. `web_search/commands.rs` owns typed operation inputs; `schema.rs` supplies
the shared code-mode catalog. Each conversation supplies its stable ID and
current model via a weak-conversation callback. Saved policy is read for every
provider catalog and native invocation. Children use their own conversation IDs and fixed profile models.

`ToolEvent` and `EntryKind::Tool` carry optional web metadata separately from output.
Complete metadata/diagnostics live in SQLite (not disposable temporary files), while
oversized agent text uses the established `/tmp` spill policy. The provider's fallback
compaction archive omits web metadata but preserves all agent-facing tool output.
The app renders source links only for expanded web entries; complete metadata
remains stored in SQLite. General settings and a web result are included in the window smoke.

Run `cargo test --locked -p pico-core web` for focused regressions and see
[web access](web-search.md) for the explicitly opt-in live acceptance command.

### Native image input

The app's image input uses a small patch to pinned `winit` 0.30.13, documented in
[`patches/winit/README.md`](../patches/winit/README.md). Nix fetches the official
release by SHA-256, applies the checked-in diff with no fuzz, and supplies the
same patched source to development and packaged builds. Only the patch and pin
are tracked; `.build-deps/winit` is generated. No X11 features are enabled.
URI parsing regressions include the exact patched backend parser in the app
tests, so they run under `just verify` without a separate dependency workspace.
Attachment tests cover draft
persistence, image-only provider projection, import/navigation serialization,
failed-send retention and hover/remove/modal hit routing. Rendered smoke includes
the attachment strip; native clipboard/source interoperability still needs the
manual acceptance checks in `images.md`.

Native input acceptance was also exercised in an isolated nested niri compositor:
Ctrl+V from an `image/png` clipboard created a durable attachment, and a real
`wl_data_source` drag containing PNG + GIF file URIs completed the Copy/finish
handshake and appended both images in order. Restart retained all three images;
ordinary text pasted exactly once; thumbnail clicks opened the GIF modal, and
the hover remove button removed just its attachment. This used separate clipboard and
configuration state, without touching the desktop clipboard or real sessions.


### Growing message composer

The composer starts at two visual lines, grows to ten, and then scrolls internally.
A scrollbar in its right padding appears only for overflow; it supports thumb
dragging and track clicks without changing the text selection. Native wheel and
cursor scrolling update the same thumb. Scrollbar height uses cached native rows
and estimates for unmeasured offscreen text; it refines as content is visited.
Absolute scrollbar seeks shape the destination logical line and viewport, preserving the
caret/selection and avoiding work proportional to the distance scrolled. Width/font changes and deletion remeasure
wrapped rows and hide the scrollbar when the text fits again.

The composer regression tests exercise retained widget state, dragging (including
batched motion), wheel/keyboard scrolling, selection preservation, exact-boundary
visibility, and single-logical-line wrapping/reflow. The final rendered smoke
frame includes overflowing draft text and image attachments.

## Compact chat activity

See [chat activity](chat-activity.md) for grouping, execution-detail and reasoning
contracts, status semantics, regression coverage and live acceptance steps. The
rendered smoke includes collapsed/live groups and expanded/narrow activity details.

### Shared UI theme

`crates/pico-app/src/theme.rs` is the developer-editable source of the app's dark
palette, semantic colors, corner radii, borders and widget styles. It also supplies
the Markdown palette; avoid hard-coded colors or `Theme::Dark` in production views.

Use `theme::button` for buttons (including those with no explicit style), and the
shared style functions for inputs, editors, pick lists and their menus, checkboxes,
cards, panels and popovers. Controls and session rows use 8 px corners; message
cards, editors and dialogs use 12 px. Tiny badges, checkboxes and inline code use
4 px; scrollbar thumbs retain their pill shape. Edge-to-edge panels, dividers and
icon geometry intentionally stay independent of card rounding. Layout dimensions
and user-configurable text sizes remain in their owning views.

Vertical scroll areas use `scrollbar::scroll`, which always reserves a 5 px
rail plus a 2 px content gap, including when content fits. Keep row backgrounds
and hit targets outside this gutter. Where possible, let it replace existing
right-side panel padding rather than stacking extra insets. Use
`scrollbar::horizontal` for horizontal scrolls; changing the direction of the
vertical helper would retain the wrong gutter. Growing editors reserve the same
gap in their text padding, and the pinned iced menu patch covers built-in
pick-list popups (see `patches/iced-widget/README.md`).

After changing visual styles, run `just verify` and, on a Wayland desktop,
`just smoke-gpu`. These exercise the existing rendered views and interaction
regressions without requiring real credentials or modifying user sessions.

## Desktop notifications

See [notifications](notifications.md) for focus policy, native delivery, failure
recovery, Wayland activation limitations and manual acceptance. Protocol tests
start a private `dbus-daemon` with a fake notification service; the development
shell and Nix package checks provide D-Bus, but the installed app uses the desktop's
existing session bus. Smoke mode never sends desktop notifications.

### Sidebar status waves

`session_wave.rs` owns the shared sidebar presentation states and widget-local
wave motion. Session grouping and row rendering use the same prepared status.
A small untextured triangle mesh reproduces the V1 wave, following each row's
rounded outline and interpolating per-vertex alpha across two moving crests.
There is no gradient-strip implementation or alternate renderer path. Work stays
bounded to visible rows. Waves never request their own redraws: they advance on
existing window redraws, normally the shared 100 ms activity Tick while work is
active. That timer also covers sidebar Working rows during terminal persistence
or child work. The wave retains its 12-second cycle; questionnaire/error states
remain frozen. Interaction/streaming redraws may update the wave more frequently.
The final smoke frame renders working, questionnaire, error, unchecked-completion
and idle rows. See [notifications](notifications.md#sidebar-status-and-unchecked-completion)
for completion-dot persistence and focus/open-to-clear behavior.

All rendering tests now use wgpu, including the formerly GPU-only pixel checks.
A shared test engine reuses one Vulkan device and its pipelines per test process;
individual fixtures retain separate renderer/widget state. Tests that previously
inspected software-renderer drawing lists instead check painted pixels or compare
retained-widget output with an independently constructed fresh view. The ordinary
suite checks wave animation, freeze/resume, clipping, text rendering and fractional
image edges on the CPU Vulkan driver. Run `just smoke-gpu` for host-GPU acceptance.

## Automatic session titles

On the first accepted user message, main sessions request a short title from
`gpt-6-luna` in the background through the existing ChatGPT subscription provider.
The tools-disabled request contains only that message (including attached images),
not project instructions or later conversation history. It does not change the
conversation's selected model, transcript, context, usage display or running status.
The existing first-message title appears immediately and stays if naming fails,
returns an unusable title, or exceeds 60 seconds. Provider transport retries still
apply within that deadline; later sends and restarts do not trigger another naming
job. Existing conversations and subagent titles are not retroactively renamed.

Manual renames are persisted explicitly and always win over pending naming,
including a rename to the same displayed text. Pending naming prevents transcript
eviction but not session deletion; deletion cancels the request and excludes late
writes. A title-save failure retains the generated title in memory for normal save
retry and adds a recoverable notice without failing the main turn.

Run `cargo test --locked -p pico-core agent::naming` for local HTTP regressions
covering parallel main responses, first-send/repeat-send behavior, manual rename,
eviction/reload, deletion, provider/validation failure and storage recovery.
Live acceptance: send a message in a new session and verify that its sidebar title
updates while the main response continues normally; rename another session while
its title request is pending and verify your manual name remains. Live naming uses
subscription usage and is not exercised by automated tests.

## Settled sessions

Idle, checked main sessions offer **✓ Settle** in place of their age on row hover.
Working sessions, pending questionnaires, errors and unread completion dots do not
offer it. Settling leaves the open conversation and draft unchanged. The bottom
Settled section starts collapsed; its compact, independently virtualized list
shows at most roughly ten rows (fewer in short windows), newest settlement first.
Row hover swaps the settlement age for Undo. Search includes settled sessions and
temporarily expands matching results without changing the normal expansion state.

An optional `settled_at` timestamp in both session metadata and sidebar summaries
is the entire durable state. Undo preserves the activity ordering; accepted
messages and automatic turn starts clear settlement. Filing is serialized with
turn/child starts and retries outstanding child-result delivery before saving.
Paused input and notices are preserved, not resumed or discarded by settlement.
Hover handling is widget-local and shares questionnaire Edit styling. The supplied
Lucide SVGs in `crates/pico-app/src/icons/` use Iced’s native SVG renderer and inherit
the enclosing button’s text color. The checkmark matches the Settle label’s font
size; Undo remains 16 px without rotation.

Run `just test settl` inside `nix develop` for persistence/retry, eligibility,
continuation, hover/click routing and bounded-list regressions. The window smoke
also renders the expanded list at its beginning and end. For manual acceptance,
settle an open checked session, expand/collapse the section, browse more than ten
rows, undo another session, and send a new message in the settled conversation.

## Compact project picker

The new-session picker uses compact badge/name/favorite rows and the shared V2
theme. Right-click a row to rename its
Pico display name, stash/restore it, or delete it. Stashing only moves the picker
entry into the collapsed Stashed projects section; conversations remain in the
sidebar. Names, favorites and stash state persist in the catalog.

Project deletion requires confirmation, refuses active turns/background tools or
pending questions, and deletes all matching conversation families in one SQLite
transaction. It never deletes the project folder. The catalog is saved after the
conversation deletion; if that later write fails, the project remains available
for a retry with a clear error. Unsent new-session text/images are preserved, but
their deleted project association is cleared.

Run `just test project_` for persistence, failure/retry, family deletion,
background-work refusal, compact layout and click-routing regressions. The window
smoke includes populated and expanded-stash pickers, the project context menu and
delete confirmation. Manual acceptance: right-click a scrolled project row, rename
it, stash/restore it, toggle its star, and cancel/confirm deletion of a disposable
project after stopping its sessions.

### Session age

Sidebar age and ordering restart on accepted user messages (including steering),
successfully submitted questionnaire answers, and successful main-session
completion. Streaming and ordinary tool activity do not reset the age, nor do
failed submissions, stopped turns or errors. Completion waits for pending
questions and child work to finish. Completion age is saved with the other
completion metadata by ordinary session saves; answer age is saved with its tool
result and retained in memory for retry if storage fails. Settled rows continue
to show time since settlement instead.
