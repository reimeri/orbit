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
- Confirm a local Markdown link such as `[report](docs/report.pdf)`,
  `[report](/tmp/report.pdf)`, or `[report](file:///tmp/report.pdf)`: it opens
  with `xdg-open` in the default application. Relative paths use the conversation's
  project folder, not Pico's launch directory; URL-encoded spaces are decoded.
  Cancel leaves the file unopened. Web links still open in the browser; unsupported
  schemes are rejected. If launching fails, the error identifies the destination
  and suggests opening it manually or checking its default application.
- Drag to select part of a message, then across paragraphs and messages in either
  direction. Ctrl+C copies the displayed text, not Markdown markup. Check code
  blocks (including blank lines), Unicode, notices and expanded thinking/tool
  details. While holding the mouse button, wheel-scroll the chat or a large tool
  output, then release and copy; offscreen selected text must be included.
- Double-click a word (including accented/RTL text and a word spanning Markdown
  formatting), then Ctrl+C. Triple-click a wrapped paragraph: all its visual lines
  should be selected, but not the neighboring paragraph. In code/tool output,
  triple-click selects the logical source line, even if it wraps beyond the visible
  rows. Plain messages use blank-line-separated paragraphs.
- Hold the second/third press and drag backward, forward, and back into the starting
  unit: selection extends by whole words/paragraphs/lines without losing the original
  unit. Repeat while wheel-scrolling a large output, then release and copy.
- Clicking a link still opens confirmation immediately, even during multi-clicks;
  dragging over it only selects text. There is no multi-click activation delay.
  Click the composer and verify its own selection/Copy works normally. Changing
  sessions or collapsing content clears the chat selection. A streaming update
  to selected content also clears it, rather than keeping stale text offsets.
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

- Choose a project from the new-session picker, type an unsent multiline message,
  and click another favorite project badge. The message follows the project, the
  model/effort reset to defaults, and no new sidebar session appears yet.
- Visit an existing session, type a separate follow-up, then return through the
  project picker. The new-session draft and existing-session follow-up stay separate.
- Close and reopen before sending the new draft. Its text survives (with resume
  enabled, its project opens too). Send it: exactly one session appears in the final
  project and the next new draft is empty. Blank sends create nothing.
- Type an unsent multiline draft in an existing session. Switch sessions and return;
  the draft survives.
- In the composer, search, settings and dialog text fields, Ctrl+Backspace and
  Ctrl+W delete the previous word (or only the selection when text is selected).
  Check punctuation, Unicode/RTL text, a cursor in the middle of text, and the
  start of the input. In the multiline composer, also check the start of a line.
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

- **401 / expired login:** the Responses provider retries one rejected token per
  request, independently of its five transient retries. For example, a 503
  followed by a 401 can still recover automatically. If the new token is also
  rejected, stop running turns, then sign out and sign in again in Settings.
  An explicit stream overload before generated output also uses the transient retry
  budget, waiting 1, 2, 4, 8, then 16 seconds. Once output begins or a completed item is
  received, Pico does not replay the request: tools may already have run.
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
- **Graphics startup:** check the host Vulkan/graphics drivers and native Wayland
  session, then retry inside `nix develop`. Pico uses wgpu only, with no software
  fallback. Use `just smoke` for an isolated CPU-Vulkan test, not as a normal app mode.

## UI regression checks

- Open the footer model picker and leave it open: ordering must stay fixed across
  redraws and status updates; Astra, Sol, Terra, and Luna appear once each, without version IDs.
- There is no chat header, bottom-sidebar New session/shortcut label, composer
  hint, Send, Continue, Latest or Stop buttons. Enter still sends; Shift+Enter adds
  a line. Esc dismisses an open menu, dialog or Settings first; with those closed,
  it stops only the selected session's running agent, including while the composer
  is focused. Check that a running agent in another session is unaffected. New
  session is available from the left-sidebar top plus and Ctrl+N. The right
  sidebar has no plus button.
- The footer stays one row at narrow widths. The project path replaces the project
  name; hover a shortened path for the full value. Hover the info indicator for
  usage details omitted due to limited width. The right-rail gear is centered.
- Expand a tool with a large output, scroll to its end, resize/open Settings, then
  scroll again. The tail must remain accessible, without blank viewport gaps.
  Copy output must copy the entire original result, not just the visible rows.
- Leave a short session idle with the window visible. It should not refresh at
  20 Hz. Confirm a running session still streams and late background tool output
  appears without clicking or switching sessions.

## Settings and model defaults

- Verify Account, General, and Models tabs remain visible while scrolling their
  content. Account owns sign-in/out; General owns the default-model dropdown,
  context/compaction settings and appearance; Models owns the per-family defaults.
- Set Astra to Max/Fast on and Luna to None/Fast off, then Save. Choose
  Astra as the default in General and apply. A new session starts with Astra,
  Max and Fast. Switch to Luna in the footer: effort becomes None, Fast turns off.
- Click anywhere on the combined model/reasoning footer button. The popup has
  models on the left, supported reasoning levels on the right, and Standard/Fast
  along the bottom. Astra/Sol/Terra have Low, Medium, High, Extra High, Max; Luna also
  has None. Changing model refreshes these choices and applies its saved defaults.
- Changes leave the popup open. Selections are mouse-only: Tab, arrows, Enter and
  Space do not navigate, select, or type/send in the composer behind the popup.
  Escape, clicking outside, or clicking the footer button dismisses it without
  changing the composer or closing Settings.
- Select Standard/Fast with the mouse. Selecting an already-active
  speed is a no-op, not a toggle. Fast uses the Lucide lightning icon; the combined
  footer button shows that icon only when Fast is enabled. Model/effort/Fast changes
  are unavailable during an active turn or while a setting is being saved.
- With both sidebars expanded, check that the unified footer target stays contained
  and the popup's two columns remain readable and within the window. The popup may
  extend over the left sidebar when the center is narrow.
- Save an existing session with non-default effort/Fast, restart, and reopen it:
  its selections persist. Changing model defaults does not overwrite it.
- In isolated test state, open a saved session with a retired model. Expect the
  configured default model and its defaults, an informational toast, unchanged
  transcript/draft, and persistence across restart. Reopening it must not warn
  again. The toast can be dismissed and otherwise expires after about 8 seconds.
- With authorized live access, compare Fast on/off requests: Fast requests
  priority service and the matching routing hint. Normal mode omits the tier
  and uses a model-only routing hint. Local tests verify the wire contract, not
  actual priority access or service latency.
