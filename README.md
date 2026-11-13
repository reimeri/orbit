# Pico V2

A native Wayland coding harness written in Rust with iced. **Phase 1 is implemented:**
project/session management, streaming Markdown chat, ChatGPT subscription login,
an agent loop, and JavaScript code mode over interactive shell tools.

## Run

Target: **x86_64 Linux / NixOS, native Wayland only**. Start from your Wayland desktop:

```sh
nix develop
just run
```

1. Open **Settings** (gear on the right) and **Sign in with ChatGPT**.
2. Complete the browser login. If browser launch fails, use **Copy login URL**.
3. Choose **Add project**, browse to a folder or enter its absolute path.
4. Select the project and send a request. Enter sends; Shift+Enter adds a newline.

**Commands run with your user privileges, without a sandbox or approval prompts.**
Use a disposable project for initial testing. Selecting a project sets the command
working directory; it does not restrict filesystem or network access.

The default model is `gpt-5.6-sol`, following the V1 reference. Model availability
is account-dependent. Settings accepts any model ID and reasoning effort; use
**Apply defaults**, then create a session or select that default in an existing
session's footer. Use a model that supports custom/freeform tools. Set its usable
input context limit in Settings; the initial limit is 272,000 tokens.

## What works

- Project picker, favorites, session search by title/project, automatic initial
  titles, right-click Rename/Delete menus with confirmations, saved drafts, restored selection.
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

There are **no extensions** and no V1 migration/compatibility layer. Phase 2 browser,
image, subagent, questionnaire, diff and statistics features are not placeholder UI.

## State and recovery

All app state is in `$XDG_CONFIG_HOME/pico-evo`, or `~/.config/pico-evo`:

- `settings.json`: projects, favorites, layout and defaults.
- `sessions/<uuid>.json`: full transcript, separate provider context, pending
  steering, draft, model and usage. Compaction does **not** delete transcript text.
- `auth.json`: OAuth credentials, mode `0600`, inside a mode `0700` directory.
- `instance.lock`: prevents two processes from overwriting the same state.

Credentials are permission-protected local files, **not encrypted at rest**.
Never share `auth.json`; transcripts/tool output can also contain sensitive data.
Writes use private temporary files, fsync and atomic replacement. Unreadable
sessions are preserved and reported, not reset. Interrupted calls are closed with
an explicit uncertainty notice: the agent must inspect effects before retrying.
JS variables, `store` values, cell IDs and shell sessions are process-local; they
are not resurrected after restart. User instructions, transcript and model context
are durable. Switching ChatGPT accounts rebuilds incompatible encrypted context
from the transcript.

No old `pico` directories or workspace `.pico/media` assets are migrated or modified.

## Verify and test your account

```sh
# Inside nix develop:
just check
just test-core
just verify       # Format, Clippy, workspace tests, rendered software UI checks
just smoke-gpu    # wgpu on your real Wayland desktop

# Packaging verification, not the everyday edit loop:
nix flake check
nix run
```

Automated tests use temporary directories and loopback HTTP fixtures—no account
or live model calls. The rendered smoke test visits the project picker, Markdown
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
