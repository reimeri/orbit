# Phase 1 manual acceptance checklist

Run `nix develop --command just run` from your Wayland desktop. Use a **disposable
project directory**: commands are unrestricted. No live credentials or prompts
were used during implementation verification.

## 1. Account and project

- Open the right-hand gear → **Sign in with ChatGPT**; finish the browser login.
  Expect the success page and the account label in Settings.
- If a browser does not launch, **Copy login URL** and open it manually. If both
  callback ports 1455/1457 are occupied, close the other login flow and retry.
- Add a project by browsing, then also test entering a path. An invalid path
  should show an error **inside the still-open modal** without adding a project.
- Choose a model available to your subscription in Settings and apply defaults.
  The model must support custom/freeform tools. Create a new session.

## 2. Streaming and real file changes

Send:

> Inspect this directory. Create a small Python module implementing a function
> that returns the median of a nonempty numeric list, plus unittest tests for
> odd/even lengths, duplicates and negative values. Run the tests. Explain the
> result and show the files changed. Use only the shell tools through code mode.

Check:

- Text/reasoning streams without freezing the interface.
- Tool rows expand to show JavaScript, command arguments and output. Expand the
  nested tool group as well as its individual rows.
- Actual files exist in the project and tests really ran. Errors, if any, are
  visible and the agent can correct them.
- Markdown/code blocks render; Copy works; links require confirmation.
- Git branch and change counts update if the folder is a repository. The footer
  reports model, effort, elapsed time and provider usage where available.

## 3. Shell and code-mode lifecycle

Ask:

> Through exec, start an interactive shell command with tty=true that prints a
> prompt, reads a line and echoes it back. Yield early, then use write_stdin to
> send "pico test" and read its final output. Also run two independent commands
> concurrently with Promise.all and print both results.

Then:

> Start a JavaScript cell that awaits a 15-second timer and then prints
> "cell complete". Give exec a short yield_time_ms, then use wait with its cell_id
> until it completes.

Expect running IDs, no repeated chunks on polling, the echoed input, and a final
cell completion. Cell IDs and shell session IDs are separate namespaces.

## 4. Steering, cancellation and concurrent sessions

- Ask for a command that sleeps for 20 seconds before printing a result.
- While it runs, send: “After this command, create `steered.txt` with the text
  `steering received`, then stop.” It should appear as pending and be consumed
  after the current batch, not be lost or applied to another session.
- Start work in a second session while the first runs. Search/select both;
  switching sessions should not cancel either.
- Run another long command and click **Stop**. Expect the working indicator to
  settle, an interruption notice and an editable composer.
- Send “Continue” with Enter. The agent must not assume interrupted commands had no side effects.

## 5. Persistence and UI

- Type an unsent multiline draft. Switch sessions and return; the draft survives.
- Right-click a session to Rename/Delete it. Check that right-clicking a different
  session acts on that session without switching away from the current conversation.
  Star a project, search by title and project, resize both panes,
  adjust font/chat width, and change the resume-last-session option.
- Close normally and reopen. Login, sessions, drafts, favorites and settings
  should persist. Shell/cell IDs do not survive restart.
- With work running, close the window: confirm cancellation, wait for saving,
  then reopen. Inspect the interrupted session before continuing.
- Test Rename/Delete/Add Project dialogs and Escape. Typing in a dialog must not
  alter the composer behind it. Deleting a session must not remove project files.
- Starting a second app instance should report that storage is already in use,
  rather than create competing writers.

## 6. Automatic compaction

For a deliberately low-cost test, set the context limit to **8000** and the
threshold to **30%**, apply defaults, and send several substantive messages or
ask for a moderate command output. Expect a “Compacting context” status and a
notice after replacement. Earlier messages/tool output remain visible. Ask a
question about an earlier decision to check continuation quality, then restart
and continue the same conversation. Restore the model's proper limit afterwards.

If the model rejects native compaction, bounded transcript summarization should
recover automatically. A connection/authentication failure must leave the original
context intact and explain how to retry. Summaries are model-generated and can
lose details; the original transcript remains the source of truth.

## Troubleshooting

- **401 / expired login:** stop turns, sign out and sign in again. Refresh is
  normally automatic; revoked credentials require a fresh login.
- **Model or effort unsupported:** set an available model/effort in Settings or
  the session footer. Model access is account-specific; static suggestions are
  not a service availability guarantee.
- **Truncated/stalled response:** send a new message to continue from committed context. No
  partial tool arguments are executed. Inspect completed side effects first.
- **Storage error:** keep Pico open, copy an unsaved draft, fix disk space or
  permissions, then retry. Failed credential rotation writes retain the new
  credentials in memory until they can be saved.
- **No folder picker:** enter the path directly. Native browsing requires your
  desktop's xdg-desktop-portal file chooser backend.
- **Graphics startup:** retry `ICED_BACKEND=tiny-skia cargo run` in `nix develop`.

## UI regression checks

- Open the footer model picker and leave it open: ordering must stay fixed across
  redraws and status updates; the selected/default custom models appear once.
- There is no chat header, bottom-sidebar New session/shortcut label, composer
  hint, Send button or Continue button. Enter still sends; Shift+Enter adds a line.
  The Stop button remains available during a turn. New session is available from
  the top plus controls and Ctrl+N.
- The footer stays one row at narrow widths. The project path replaces the project
  name; hover a shortened path for the full value. Hover the info indicator for
  usage details omitted due to limited width. The right-rail gear is centered.
- Expand a tool with a large output, scroll to its end, resize/open Settings, then
  scroll again. The tail must remain accessible, without blank viewport gaps.
  Copy output must copy the entire original result, not just the visible rows.
- Leave a short session idle with the window visible. It should not refresh at
  20 Hz. Confirm a running session still streams and late background tool output
  appears without clicking or switching sessions.
