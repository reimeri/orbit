# Pico V2

Main rule for the overhaul to V2: Do not take the old implementation as any way sacred, you are free to do very radical changes to the code base as long as they improve the quality of the code. Remove anything that is not needed. No backwards compatability between earlier versions and V2

Do not try to directly translate the C code to Rust. Use idiomatic rust instead of any hybrid approaches

## What is working

- Session and workspace format on disk
- Generally session management works quite well
  - Some features like session forking, session tree are missing and will be needed to be added in the future
- UI layout is generally good

## What most likely should be overhauled

- Agent loop. It is working but we should consider overhauling it. Many harnesses have moved to using code-mode which I think should be also our target
- Subagents. Make them closer to how they work in codex
- Shell. We should use the same exec_command and write_stdin tool calls that the codex harness uses
  - This makes the \*\_background tools not needed
- Proper web search tool for the agent. Would use the web search capabilities of openai models.

## What needs to be removed completely

- Extensions. No extension support. All features are buit-in

## Implementation phases

During the development the default folder of pico V2 in .config and such places should be called pico-evo to avoid conflicts with the previous version of pico.

### Phase 0 — Development foundation

- [x] Pinned Nix flake: Rust development shell and runnable package
- [x] Rust workspace with an iced app and UI-independent core crate
- [x] Native Wayland-only window; wgpu-only rendering (CPU Vulkan driver for automated tests)
- [x] Isolated `pico-evo` application/configuration identity (no persistence yet)
- [x] Formatting, linting, unit tests and headless rendered-window smoke check
- [x] Real desktop GPU window check
- [x] Incremental-build defaults, fast linker and measured development workflow

See [README.md](README.md) for quick start and [docs/development.md](docs/development.md) for compilation guidance. This phase does not implement the Phase 1 harness.

### Phase 1

Implemented. Live account acceptance steps: [docs/phase-1-testing.md](docs/phase-1-testing.md).

- [x] Main UI
  - [x] We should generally keep the look and feel of the UI
  - [x] Left bar: session picker, search, new session button, workspace badge favorites, animations, resizing
  - [x] Footer: include all the info that it currently shows
  - [x] Right bar: settings button
  - [x] New session view with add project button
  - [x] Settings panel
  - [x] Chat with markdown rendering and clickable tool groups and tool rows
  - [x] Modals
- [x] Shell tools exec_command and write_stdin
- [x] Agent loop
- [x] OpenAI provider
  - [x] Oauth login so that you can use chatgpt subscription in the harness
  - [x] Login details persisted over restarts
- [x] code mode: exec and wait tools in the top-level


### Phase 2

- Other tools
  - [x] Subagents — see [tool contract, lifecycle and acceptance checks](docs/subagents.md)
    - Async parallel child conversations, isolated from parent context
    - `spawn_subagent`, `wait_subagent`, `list_subagents` inside code mode
    - Parent-owned child-session continuation; separate persistent assignment IDs
    - Six running children per parent; no nested delegation
    - Final-only repeatable results, durable completion notices and idle-parent wake-up
    - Agents panel, live child transcript inspection, scoped Stop and family deletion
    - Review (`gpt-6.1-sol`, medium) and Explore (`gpt-6-luna`, high)
    - Both profiles are read-only by instruction, not filesystem sandboxing
  - [x] Questionnaires — see [tool contract and acceptance checks](docs/questionnaires.md)
    - Built-in `tools.ask_user` for main conversations, with model continuation paused until answered
    - V1-style select/text panel, editable options, custom answers and keyboard navigation
    - Session-local drafts, Collapse/Resume, scoped cancellation; no clarification conversation
    - Lossless large answers without V1's answer byte/line limits
  - [x] Image viewing and image generation — see [contracts, settings and acceptance](docs/images.md)
    - Codex-style unified `view_image` for main and child agents
    - Main-only `image_gen__imagegen`, with required `output_path`, PNG/GIF generation and editing
    - Codex subscription or explicit OpenAI API billing; private Account API-key setting
    - Images settings: editable API model ID, shared quality, automatic size
    - Durable expanded-result thumbnails; full-size modal with GIF Play/Pause
    - Atomic no-overwrite output publication, explicit failures and paid-result recovery
    - Model-update skill maintains the default API image model without replacing saved IDs
  - [x] Remaining code-mode helpers: `image`, `audio`, `content`, `generatedImage`
    - Ordered typed media through yielding, persistence, replay and compaction
    - Explicit unsupported-audio capability errors; no audio recording/playback tools
  - [x] Web access — see [contract, settings and acceptance](docs/web-search.md)
    - Main and subagent `tools.web__run` with all ten Codex standalone operations
    - ChatGPT subscription auth; Live/Cached/Disabled policy, no billing fallback
    - Text to JavaScript; durable metadata/source links for transcript UI only
    - Live Rust/QuickJS acceptance and local failure/lifecycle regression coverage
  - Browser tools
  - Use skill
- UI
  - [x] Native desktop notifications for main-agent questions, completion and terminal errors — see [behavior and verification](docs/notifications.md)
  - Integrated browser
  - Integrated diff viewer
  - Statistics

### Phase 3

- Polish

## References

You can find Pico V1 source code at `./pico-reference` folder.
