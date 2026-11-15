# Questionnaires

`tools.ask_user` is a built-in **main-conversation-only** code-mode tool. Review
and Explore do not receive it, and the native dispatcher rejects attempts to call
it from children. Only `exec` and `wait` remain top-level provider tools.

```js
text(await tools.ask_user({
  questions: [
    {id: "target", question: "Which interface?", kind: "select",
     options: ["Website", "Desktop application"]},
    {id: "constraints", question: "Describe the constraints.", kind: "text"}
  ]
}));
```

Every question is required. The result is a JSON **string**, in question order:

```json
{"answers":[{"id":"target","answer":"Desktop application"},{"id":"constraints","answer":"Native Wayland on Linux."}]}
```

## Validation and limits

- 1–24 questions with unique, nonblank IDs of at most 128 UTF-8 bytes.
- Nonblank question text; `kind` is `select` or `text`.
- Select questions require 1–20 nonblank string options. The UI always adds
  **Custom answer…**; agents must not include it in `options`.
- Text questions ignore `options`. Unknown request/question fields are rejected.
- The complete serialized request is limited to **1 MiB**, checked before parsing.
- Answers must contain non-whitespace text. Otherwise their contents are preserved,
  including leading/trailing whitespace and multiline text.
- **No questionnaire-specific answer byte or line cap.** V1's 16 KiB answer,
  64 KiB aggregate-answer and fixed 512-line wrapping limits are not carried over.
  The structural limits bound UI complexity, not C buffer allocation.

The full answer JSON is retained in the nested tool transcript. The copy delivered
to JavaScript is saved in full to a private `/tmp` file when it exceeds 40,000
Unicode characters (the normal 10,000-token inline budget). It then returns a
file notice with the absolute path and exact byte size, not a clipped JSON value.
Read the file with shell tools. The outer `exec`/`wait` budget applies independently.
Files remain after cell/app exit; Pico does not clean them up. If file preservation
fails, the complete result is returned inline with a warning. Normal process and
QuickJS memory limits still apply; an unbounded answer policy is not a guarantee
of unlimited physical memory.

## Runtime lifecycle

- A request belongs to its unique nested-tool call and code-cell cancellation token,
  not to the currently selected UI session.
- Tool-start persistence must succeed before a panel opens.
- Awaiting user input is event-driven and has no questionnaire timeout. Ordinary
  `exec` yielding applies; the model may continue and must use `wait` to collect its
  cell's result. A model response ending does not itself discard a pending cell.
- Multiple concurrent asks queue in arrival order within each conversation. Different
  conversations remain independent. Submission routes by request ID; stale/duplicate
  submissions are rejected.
- Unawaited promises do not keep a completed cell alive. Cell completion or
  `wait({cell_id, terminate:true})` cancels that cell's pending questionnaires.
- Escape uses the existing scoped Stop behavior, cancelling the selected conversation
  and its children. A menu/modal/search/settings/Agents panel gets Escape first.
- Requests keep their conversations resident across selection changes. A failed
  result write retains the complete answer in the in-memory transcript, reports the
  failure, and allows normal session saving/flush to retry. Keep Pico open until
  storage is fixed; do not blindly repeat the tool.
- Pending requests and UI drafts are **not persisted**. After a restart, unfinished
  `ask_user` rows are marked cancelled, with an explanatory notice. This includes a
  model turn that finished while its ask cell was still waiting. Already submitted,
  saved answers remain available. There is no cell-resumption or V1 migration layer.

## UI behavior

The non-modal, rounded dark **Answer needed** panel replaces the composer. Chat,
selection/copy, sidebar navigation, search and settings remain available. Sessions
with pending questions appear under **Needs attention**, ahead of Working.

- One question at a time; Back/Next preserves all answers. The final button is Submit.
- No option is selected initially; blank text disables Next/Submit.
- Each predefined option has **Edit**. Edit/E seeds Custom answer with the option
  text and puts the caret at the end, without changing the original option or
  advancing. A different nonempty custom draft requires **Keep draft / Replace**,
  defaulting to Keep draft. Typing during this confirmation does not edit the draft.
- The centered pill toggles collapse/expand without changing shape, retaining the
  current question, answers, and replacement confirmation.
- The panel and options use the shared app palette. Hover highlights the entire
  option, including behind Edit; Edit is muted until hovered. Option numbers are
  vertically centered alongside their labels.
- Switching sessions preserves questionnaire drafts and the ordinary composer draft.
- Long prompts/options scroll in a bounded body; the multiline editor, keyboard hints
  and navigation remain accessible outside it. The editor scrolls independently.
- Submitted answers appear in the existing tool result, with no synthetic user message
  or separate answer-summary entry.
- There is deliberately **no Ask about this button or clarification conversation**.

While the expanded panel is focused:

| Keys | Action |
| --- | --- |
| Up/Down, 1–9, 0 for option 10 | Select an option |
| E | Edit selected predefined option |
| Enter / Tab | Next or Submit |
| Shift+Tab | Back |
| Shift+Enter | Newline in text |
| Ctrl+Backspace / Ctrl+W | Delete previous word, or selected text |
| Escape | Scoped Stop, unless higher-priority UI handles it |

Within text, letters, digits and arrows edit the answer. Text uses the same iced
editor and logical Unicode word-deletion implementation as the composer, including
selection/copy/cut/paste, word movement, Home/End, Shift selection and mouse selection.
In replacement confirmation, Tab or Left/Right switches actions; Enter activates.
Questionnaire shortcuts stop when focus moves outside the panel or it is collapsed.

## Verification

Run within `nix develop`:

```sh
cargo test --locked --workspace questionnaire
just verify
just smoke-gpu # on a real Wayland desktop
```

Tests cover boundary validation, ordered/custom/large answers, actual code-mode
round trips, capability enforcement, cell cleanup/cancellation, FIFO routing,
persistence failures, restart cancellation, session switching, draft retention,
keyboard focus/navigation and both Unicode-aware word-deletion shortcuts.
Smoke frames include select, seeded editor, replacement confirmation, collapsed,
large multiline text and long options with Settings open. When inspecting seeded
text, also inspect the actual compositor window: iced's screenshot task can omit
live editor glyphs even though they are correctly drawn on screen.

Live acceptance (requires a signed-in account):

1. Ask the main agent to use `tools.ask_user` with a select and a text question.
   Verify the V1-style panel and absent clarification button.
2. Select using click, arrows and digits; Edit/E an option. Type a custom answer,
   edit another option, and exercise both Keep draft and Replace.
3. Try Ctrl+Backspace and Ctrl+W in the middle of a line and with a selection;
   verify suffix preservation, Unicode and multiline clipboard editing.
4. Use Shift+Enter, Back, Next, collapse/expand and switch to another session.
   Return and verify both questionnaire and ordinary composer drafts remain intact.
5. Submit and verify the model receives the complete ordered answers through its
   waiting cell; inspect the nested tool result. No extra user message should appear.
6. Queue two parallel asks and answer them in order. Terminate one owning cell and
   verify only its questions disappear. Stop/Escape must cancel remaining work.
7. Paste an answer larger than 64 KiB. Verify the complete transcript result and
   private output-file contents, not a truncated prefix.
8. Open Settings, search and menus while a question is open. Confirm their keyboard
   input and Escape priority, and that question shortcuts do not steal text input.
9. Restart with an unanswered question. Expect cancellation and a notice, not a
   restored draft or a permanently pending tool row.

Rendered smoke checks are not a substitute for a live provider/account round trip
or manual clipboard/IME testing on the user's desktop.
