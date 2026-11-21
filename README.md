# Orbit

Orbit is a coding agent harness made for OpenAI models. It was created because the
codex CLI is limited by the terminal and the ChatGPT desktop app is slow garbage.

## Features

- Native rust executable
  - Extremely fast
  - light on system resources
- Code mode with essential tools
  - Shell (for reading and writing)
  - Questionnaires
  - Image viewing and generation
  - Web search
- Subagents
- File tags and attachements
- Spell checking
- Markdown and LaTEX rendering

## Develop

On **x86_64 Linux with Nix and a Wayland desktop**, run `nix run` from the
checkout. Once installed, launch **Orbit** from the desktop menu or run `orbit`.
For development:

```sh
nix develop
just run
```

## Install

TODO

## Directories & Configuration

Orbit stores state under `$XDG_CONFIG_HOME/orbit` (default `~/.config/orbit`):

| Path                              | Contents                                                                    |
| --------------------------------- | --------------------------------------------------------------------------- |
| `settings.json`                   | Projects, favorites, layout and defaults                                    |
| `sessions.sqlite3`                | Sessions, drafts, transcripts, provider context, forks and subagent results |
| `media/`, `attachments/`          | Saved tool media and snapshots of images attached to prompts                |
| `auth.json`, `image-api-key.json` | OAuth credentials and the optional image API key                            |
| `instance.lock`                   | Prevents concurrent writes by two app instances                             |
