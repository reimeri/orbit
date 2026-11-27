# Questionnaires

Orbit has two **main-conversation-only** code-mode tools with the same question schema:
use `tools.ask_user` when the answer blocks progress, or `tools.ask_user_async`
when the agent can keep working. Review and Explore cannot call either tool. Only `exec` and `wait` remain top-level provider tools.

```js
text(await tools.ask_user({
  questions: [
    {id: "target", question: "Which interface?", kind: "select",
     options: ["Website", "Desktop application"]},
    {id: "constraints", question: "Describe the constraints.", kind: "text"}
  ]
}));
```

Every question is required on submission. The blocking tool’s result is a JSON **string**, in question order:

```json
{"answers":[{"id":"target","answer":"Desktop application"},{"id":"constraints","answer":"Native Wayland on Linux."}]}
```

## Async questionnaires

```js
text(await tools.ask_user_async({
  questions: [{id: "audience", question: "Who will read the report?", kind: "text"}]
}));
```

The tool returns the JSON string `{"accepted":true}` once the request is saved.
There is nothing to poll or collect. The ordinary **Question** panel opens;
blocking requests take priority over async ones without resetting an answer in
progress. Within each group, requests are shown in posting order.

The request belongs to the conversation. Finishing or stopping a turn, switching
sessions, or restarting Orbit does not dismiss it. Restoring it does not start
a turn or replay a notification. Partially edited answers and undo history are
in-memory UI drafts, not saved answers.

- **Submit** sends one user message with the request ID, question IDs and prompts,
  and complete answers in order. It steers a running agent at the next response
  boundary, or starts a new turn if the agent has finished or stopped.
- **Dismiss** removes the request without sending a message or starting a turn.
  Escape still stops agent work; it does not dismiss the request.
- Submitting or dismissing leaves the ordinary composer draft alone. Answers are
  plain text, not composer file tags or skill mentions. If saving fails, the
  panel retains the draft for retry; a committed reply cannot be sent twice.

## Validation and limits

- 1–24 questions with unique, nonblank IDs of at most 128 UTF-8 bytes.
- Nonblank question text; `kind` is `select` or `text`.
- Select questions require 1–20 nonblank string options. The UI always provides
  a custom answer field; agents must not include it in `options`.
- Text questions ignore `options`. Unknown request/question fields are rejected.
- The complete serialized request is limited to **1 MiB**, checked before parsing.
- Answers must contain non-whitespace text. Otherwise their contents are preserved,
  including leading/trailing whitespace and multiline text.
- Answers have no questionnaire-specific byte or line cap.

For blocking asks, the full answer JSON is retained in the nested tool transcript. The copy delivered
to JavaScript is saved in full to a private `/tmp` file when it exceeds 40,000
Unicode characters (the normal 10,000-token inline budget). It then returns a
file notice with the absolute path and exact byte size, not a clipped JSON value.
Read the file with shell tools. The outer `exec`/`wait` budget applies independently.
Files remain after cell/app exit; Orbit does not clean them up. If file preservation
fails, the complete result is returned inline with a warning. Normal process and
QuickJS memory limits still apply; an unbounded answer policy is not a guarantee
of unlimited physical memory.

## Blocking questionnaire lifecycle

- A request belongs to its unique nested-tool call and code-cell cancellation token,
  not to the currently selected UI session.
- Tool-start persistence must succeed before a panel opens.
- Awaiting user input is event-driven and has no questionnaire timeout. Code cells
  may yield internally, but the harness holds `exec`/`wait` results while the
  conversation has unanswered questionnaires. The turn stays active with **Answer
  needed**, without model polling or completion notifications. Submission releases
  the result flow automatically; the model uses `wait` to collect a yielded cell's
  remaining output as usual. Stop cancels the wait.
- Multiple concurrent asks queue in arrival order within each conversation. Different
  conversations remain independent. Submission routes by request ID; stale/duplicate
  submissions are rejected.
- Unawaited promises do not keep a completed cell alive. Cell completion or
  `wait({cell_id, terminate:true})` cancels that cell's pending questionnaires.
- Escape uses the existing scoped Stop behavior, cancelling the selected conversation
  and its children. A menu/modal/search/settings/Agents panel gets Escape first.
- Requests keep their conversations resident across selection changes. A failed
  result write retains the complete answer in the in-memory transcript, reports the
  failure, and allows normal session saving/flush to retry. Keep Orbit open until
  storage is fixed; do not blindly repeat the tool.
- Pending requests and UI drafts are **not persisted**. After a restart, unfinished
  `ask_user` rows are marked cancelled, with an explanatory notice. Already submitted,
  saved answers remain available.

## UI behavior

The non-modal **Question** panel replaces the composer. Chat,
selection/copy, sidebar navigation, search and settings remain available. Sessions
with pending questions appear under **Needs attention**, ahead of Working.

- The header shows progress and previous/next arrows. Moving forward requires a
  valid answer; only the footer’s final **Submit** sends the questionnaire.
- No option is selected initially. Click the inline editor or pencil to use a custom
  answer; selecting a numbered option leaves the custom draft intact. Blank
  custom text disables Next/Submit.
- Each predefined option has **Edit**. Edit/E seeds Custom answer with the option
  text and puts the caret at the end, without changing the original option or
  advancing. A different nonempty custom draft requires **Keep draft / Replace**,
  defaulting to Keep draft. Typing during this confirmation does not edit the draft.
- The header collapse/resume control hides the body and footer, retaining the
  current question, answers, and replacement confirmation.
- Each answer keeps its own in-memory text edit history across question navigation,
  collapse, and session switches. Undoing **Edit** or **Replace** restores the prior
  custom text (empty if there was none), not the selected preset. History goes
  away with the questionnaire; undo cannot retract a submission.
- Switching sessions preserves questionnaire drafts and the ordinary composer draft.
- Long prompts/options scroll in a bounded body. Hover **Question** for keyboard hints.
  The editor grows with multiline text, then scrolls without moving the selection.
- For blocking asks, submitted answers appear in the existing tool result, with no synthetic user message
  or separate answer-summary entry.

While the expanded panel is focused:

| Keys | Action |
| --- | --- |
| Up/Down, 1–9, 0 for option 10 | Select an option |
| E | Edit selected predefined option |
| Enter / Tab | Next or Submit |
| Shift+Tab | Back |
| Shift+Enter | Newline in text |
| Ctrl+Z / Ctrl+Y | Undo / redo a text edit in the focused answer editor |
| Ctrl+Backspace / Ctrl+W | Delete previous word, or selected text |
| Escape | Scoped Stop, unless higher-priority UI handles it |

Within text, letters, digits and arrows edit the answer. Text uses the same iced
editor and logical Unicode word-deletion implementation as the composer, including
selection/copy/cut/paste, word movement, Home/End, Shift selection and mouse selection.
In replacement confirmation, Tab or Left/Right switches actions; Enter activates.
Questionnaire shortcuts stop when focus moves outside the panel or it is collapsed.

For local validation and UI tests, run `just test questionnaire` inside
`nix develop`. To check live continuation, leave an ask open past the cell yield
deadline. The agent should wait without polling, then receive the ordered answers
after submission without an extra user message.
