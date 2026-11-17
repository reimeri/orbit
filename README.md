# Pico V2

A native Wayland coding harness written in Rust with iced. **Phase 1 is implemented:**
project/session management, streaming Markdown chat, ChatGPT subscription login,
an agent loop, and JavaScript code mode over interactive shell tools. Phase 2 now
also includes asynchronous Review/Explore subagents, questionnaires, image
viewing/generation, and multimodal code-mode helpers.

## Run

Target: **x86_64 Linux / NixOS, native Wayland only**. Start from your Wayland desktop:

```sh
nix develop
just run
```

`nix develop` also prepares the locally patched, checksum-pinned winit dependency.
Only the [patch and pin](patches/winit/README.md) live in this repository; upstream
source stays in the Nix store, linked through the generated `.build-deps/` folder.

1. Open **Settings** (gear on the right) and **Sign in with ChatGPT**.
2. Complete the browser login. If browser launch fails, use **Copy login URL**.
3. Choose **Add project**, browse to a folder or enter its absolute path.
4. Select the project and send a request. Enter sends; Shift+Enter adds a newline.

**Commands run with your user privileges, without a sandbox or approval prompts.**
Use a disposable project for initial testing. Selecting a project sets the command
working directory; it does not restrict filesystem or network access.

Settings has **Account**, **General**, and **Models** tabs. The fixed catalog is
**Astra**, **Sol** (the default), **Terra**, and **Luna**. Choose the app default
from the dropdown in General. Models has per-model effort and Fast defaults;
initially all use Medium with Fast off. Use **Save** to save these
changes. They take effect only after the write succeeds. Save closes Settings
unless you have continued editing or reopened the panel. Reopening during a
save shows the submitted draft; a failed save leaves your edits available to
retry. Cancel discards unsaved edits, not a write already in flight. Appearance
controls apply immediately.

The footer selects a session's model and effort; its effort menu also toggles
**Fast mode**. Switching models applies the new model's defaults. Saved sessions
keep their selections; opening a session whose model has left the catalog
replaces it with the app default and shows an informational toast. The replacement
and its effort/Fast settings are saved without changing the transcript or draft.

Fast requests priority service using `service_tier: "priority"` and a matching
Codex routing hint. Model access, priority availability, and usage limits depend
on your ChatGPT subscription. Set the usable input context limit in General;
the initial limit is 272,000 tokens. The repo-local
[model-update skill](.agents/skills/update-openai-models/SKILL.md) describes how to
research and replace these families with newer versions.

## What works

- Project picker, favorites, session search by title/project, automatic initial
  titles, right-click Rename/Delete menus with confirmations, saved drafts, restored selection.
  Choosing a project opens a shared new-session draft; no session is created until
  the first message is sent. Its text follows project changes and survives restarts.
  Switching projects resets model/effort/Fast to defaults; existing sessions retain
  their own separate unsent messages.
- Dark three-column layout, resizable sidebar/settings panes, animated activity,
  Markdown and code blocks, link confirmation, copy actions, expandable reasoning,
  tool groups and individual tool inputs/results. Large tool details are virtualized: only
  visible rows are shaped, while scrolling and Copy retain access to the complete output.
- Single-row footer status, project path (full path on hover), Git branch/change counts, elapsed time, context/output
  usage, cache percentage, average turn throughput, model and reasoning effort.
  Narrow windows hide lower-priority details, which remain available on hover.
- Concurrent sessions. Sending while a turn is working **steers it after the
  current response/tool batch**. Stop cancels the turn and its command sessions;
  already executed commands are not undone. Send a new message to continue from
  saved context after a stopped or failed turn.
- ChatGPT OAuth with PKCE/state validation, local browser callback, persisted
  private credentials, serialized refresh and one refresh on rejected tokens.
- Streaming Responses requests, bounded transient-service retries, cancellation,
  incomplete-stream rejection, encrypted reasoning replay and automatic context
  compaction. Native compaction falls back to bounded transcript summarization.
- Only `exec` and `wait` are top-level model tools. `exec` accepts **raw JavaScript**,
  not JSON. Each cell gets a fresh embedded QuickJS context with async calls to
  `tools.exec_command` and `tools.write_stdin`. Running cells yield IDs for `wait`.
  Shell sessions support polling, PTY input, output limits and cancellation.

- Async [Review and Explore subagents](docs/subagents.md), with persistent child
  sessions, six parallel assignments per parent, resumable context, repeatable
  final results and completion-driven parent wake-up. Open **Agents** on the right
  rail to inspect their live transcripts. Both profiles are read-only by
  instruction, **not sandboxed**.

There are **no extensions** and no V1 migration/compatibility layer. Remaining
Phase 2 browser, diff, skill-loading and statistics features remain unimplemented;
there is no placeholder UI for them.

## State and recovery

All app state is in `$XDG_CONFIG_HOME/pico-evo`, or `~/.config/pico-evo`:

- `settings.json`: projects, favorites, layout and defaults.
- `sessions.sqlite3` (and its live WAL): indexed session metadata, full transcript,
  separate provider context, pending steering, drafts, usage, child ownership and
  durable subagent assignments/results. Compaction does
  **not** delete transcript text. Historical `sessions/*.json` files are left
  untouched but are not migrated or loaded by this format.
- `auth.json`: OAuth credentials, mode `0600`, inside a mode `0700` directory.
- `instance.lock`: prevents two processes from overwriting the same state.

Credentials are permission-protected local files, **not encrypted at rest**.
Never share `auth.json`; transcripts/tool output can also contain sensitive data.
Session changes use incremental SQLite transactions with WAL and full synchronous
durability. Settings/credentials use private temporary files, fsync and atomic
replacement. Unreadable state is preserved and reported, not reset. Close Pico
before copying its database files for a backup; a live WAL must not be omitted. Interrupted calls are closed with
an explicit uncertainty notice: the agent must inspect effects before retrying.
JS variables, `store` values, cell IDs and shell sessions are process-local; they
are not resurrected after restart. User instructions, transcript and model context
are durable. Switching ChatGPT accounts rebuilds incompatible encrypted context
from the transcript.

See [performance](docs/performance.md) for lazy loading, rendering, durability
boundaries and reproducible large-session measurements.

## Verify and test your account

```sh
# Inside nix develop:
just check
just test-core
just test         # All tests, including wgpu rendering via a CPU Vulkan driver
just verify       # Format, Clippy, tests, memory checks and Wayland smoke
just smoke-gpu    # wgpu on your real Wayland desktop

# Packaging verification, not the everyday edit loop:
nix flake check
nix run
```

Automated tests use temporary directories and loopback HTTP fixtures—no account
or live model calls. Rendering tests and `just smoke` use wgpu with the pinned Mesa
Lavapipe CPU Vulkan driver, so no hardware GPU is needed. Normal app startup and
`just smoke-gpu` use the host graphics drivers; Pico has no software-renderer fallback.
The rendered smoke test visits the project picker, Markdown
chat/tool details, settings and a modal using isolated temporary application state.

**Live OAuth and model access are intentionally left for you to test.** Follow
[the Phase 1 acceptance checklist](docs/phase-1-testing.md), including streaming,
shell/PTYS, steering, Stop, restart persistence and compaction.

## Layout

- `crates/pico-app`: iced application, UI state, platform folder picker.
- `crates/pico-core`: storage, authentication, provider, agent, shell and code mode;
  no iced dependency.
- `scripts/smoke-wayland.sh`: isolated headless Wayland window check.
- `flake.nix` / `flake.lock`: pinned development shell and runnable package.
- `docs/development.md`: incremental builds, verification and troubleshooting.
- `docs/code-mode.md`: tool contract, runtime limits and upstream references.
- `pico-reference/`: ignored V1 reference; not a dependency or packaging input.

### Image tools

[Image tools and settings](docs/images.md) are built in: main/child `view_image`,
main-only `image_gen__imagegen` with required caller-owned output paths, PNG/GIF
generation/editing through Codex subscription or explicit OpenAI API billing,
and `image`/`audio`/`content`/`generatedImage` code-mode helpers. Expanded results
show durable thumbnails with an in-app full-size/animated GIF viewer.
Settings → Images selects backend, API model and quality; Account stores a masked
private API key separately from OAuth. Current chat models do not accept audio;
the helper reports this explicitly. Live generation acceptance consumes usage and
is documented separately from credential-free fixture tests.
