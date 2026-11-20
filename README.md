# Pico V2

Pico is a native Wayland coding assistant written in Rust with iced. Sign in with
a ChatGPT subscription to work on local projects in persistent chat sessions.

## Run

On **x86_64 Linux with Nix and a Wayland desktop**, run `nix run` from the
checkout. For development:

```sh
nix develop
just run
```

1. Open **Settings → Account → Sign in with ChatGPT**. If the browser does not
   open, use **Copy login URL**.
2. Choose **Add project**, browse to a folder or enter its absolute path.
3. Select the project and send a request. Enter sends; Shift+Enter adds a newline.
   In the focused composer, Ctrl+Z undoes a text edit and Ctrl+Y redoes it.
   History follows recent drafts while Pico is open; sending clears it. These
   shortcuts do not change image attachments or undo a sent message.

**Commands run with your user privileges, without a sandbox or approval prompts.**
The project sets the working directory; it does not restrict filesystem or network
access. Use a disposable project for initial testing.

## Working with sessions

- Selecting a project opens a draft. Its text follows project changes and survives
  restarts; a session is created on first send. Existing sessions have separate drafts.
  A background request to Luna uses the first message (including attached images)
  to suggest a title. You can rename it; a manual title wins.
- Sessions run concurrently. Sending to a running session steers it **after the
  current response and tool batch**. Stop cancels the turn and its commands; it
  does not undo changes. Send another message to continue from saved context.
  A [fork](docs/session-forks.md) starts a separate conversation at an earlier
  prompt, not a separate copy of the working directory.
- Hover over an idle, checked session to **Settle** it. If it still has running
  shell commands, Pico asks before stopping them. Find it in search or expand
  **Settled** and choose **Undo** to return it to the main list.
- Right-click a project to rename, stash or delete it. Stashing hides the project
  in the picker but keeps its conversations. Deleting it removes its sessions,
  **not** the project folder.

Select a model, reasoning effort and **Fast** (priority service) in the composer.
Changing models applies the new model's defaults. Set the default model and
usable input context limit in **Settings → General**; set per-model effort and
Fast defaults in **Models**, then **Save**. Appearance changes apply immediately.
If a session's model is retired, Pico switches it to the app default without
changing the draft or transcript. Access, priority availability and usage limits depend on your subscription.

## Chat and tools

- Attach [workspace files with `@`](docs/file-tags.md) or [paste and drop images](docs/images.md).
  [Spell check](docs/spellcheck.md) runs locally.
- Expand [chat activity](docs/chat-activity.md) to inspect reasoning, commands and
  results. Select transcript text and use Ctrl+C to copy. Assistant and reasoning
  messages also render block equations: start a line with `\[` or `$$` and end
  the block with `\]` or `$$` respectively. Click an equation to copy the LaTeX.
  Inline math and unsupported equations remain source text.
- [Code mode](docs/code-mode.md) runs JavaScript over shell and other built-in
  tools. [Web access](docs/web-search.md) is always live for main agents and subagents.
  [Image generation](docs/images.md#generate-and-edit) can use your subscription
  or a separately billed API key.
- Open **Agents** to inspect [Review and Explore subagents](docs/subagents.md).
  They are read-only by instruction, **not sandboxed**. Answer
  [questionnaires](docs/questionnaires.md) in the main session;
  [desktop notifications](docs/notifications.md) signal when input is needed or
  a turn finishes outside the focused conversation.

## State and recovery

Pico stores state under `$XDG_CONFIG_HOME/pico-evo` (default `~/.config/pico-evo`):

| Path | Contents |
| --- | --- |
| `settings.json` | Projects, favorites, layout and defaults |
| `sessions.sqlite3` and its live WAL | Sessions, drafts, transcripts, provider context, forks and subagent results |
| `media/`, `attachments/` | Saved tool media and snapshots of images attached to prompts |
| `auth.json`, `image-api-key.json` | OAuth credentials and the optional image API key |
| `instance.lock` | Prevents concurrent writes by two app instances |

Close Pico before backing up the directory; do not omit a live WAL or the media
files. Credentials are permission-protected, **not encrypted at rest**. Do not
share credential files; transcripts and tool output can also contain sensitive data.

Compaction shortens provider context, not the transcript. Switching accounts
rebuilds incompatible encrypted context from the transcript. After a restart,
interrupted tools are marked rather than replayed: inspect their effects before
retrying. JavaScript store values, cells and shell sessions do not survive restart.
Historical `sessions/*.json` files are neither loaded nor migrated.

For build commands, tests and troubleshooting, see [Development](docs/development.md).
