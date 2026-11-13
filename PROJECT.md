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
- [x] Native Wayland-only window; wgpu with tiny-skia software fallback
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
  - Subagents
  - Questionaires
  - Image viewing
  - Browser tools
  - Use skill
  - Image generation
- UI
  - Integrated browser
  - Integrated diff viewer
  - Statistics

### Phase 3

- Polish

## References

You can find Pico V1 source code at `./pico-reference` folder.
