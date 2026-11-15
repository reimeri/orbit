# Desktop notifications

Pico sends native Linux desktop notifications for **main conversations**:

- **Answer needed** when an `ask_user` questionnaire reaches the front of its queue.
  A questionnaire with multiple questions produces one notification, not one per field.
- **Agent finished** when the main turn finishes successfully, with no pending
  questionnaire or running subagent assignments. Intermediate tool yields and
  automatic parent wake-ups are not completions.
- **Agent needs attention — an error occurred** for a terminal agent failure or
  failure to save the final session state.

Subagents never notify directly. User-requested Stop, recoverable tool failures,
code-mode `notify()` output, historical transcript loading and startup recovery
are not desktop-notification events. Completion describes the main agent turn;
independently launched background commands are not a separate notification source.

Notifications are suppressed **only when Pico has keyboard focus and that exact
conversation is selected**. Other conversations notify even while Pico is focused;
all conversations can notify while it is unfocused. Suppressed events are not
replayed when focus changes. Viewing a child is not viewing its parent.

The summary contains the session title; the body contains only the generic reason
above. No question, response, tool output, filesystem path or error detail is
copied into the notification body. Desktop Do Not Disturb, expiration, history and
sound policy remain under the notification server's control. Pico adds no settings,
notification history or notification controls to its GUI.

Clicking a notification selects its conversation, preserving drafts through the
normal serialized navigation path. Pico requests native window attention, but
**raising an existing Wayland window is not guaranteed**: the current iced/winit
API cannot apply the notification server's activation token to that surface.
Deleted, answered, stopped and superseded notifications cannot navigate to stale
work. Desktops without notification action support still display the notification.

## Lifecycle and delivery

`pico-core/src/agent/notifications.rs` keeps live, uniquely identified attention
signals separate from the coalesced UI refresh stream. Events remain available
until consumed, with superseded events invalidated. Terminal events survive
in-process transcript eviction but are not persisted or replayed across restarts.
Notification delivery is independent of the existing error acknowledgement state.

`pico-app/src/notifications.rs` applies focus/selection policy and delivers via
`org.freedesktop.Notifications` on the session bus, identifying the desktop entry
as `pico-evo`. One notification per conversation is replaced as its reason changes.
Answered/cancelled questions, resumed/deleted sessions and conversations brought
into focus are withdrawn where the desktop supports it. Dismissing or expiring a
notification does not cause repeated delivery.

D-Bus calls run asynchronously with a five-second timeout. If delivery fails, Pico
retains the latest still-actionable events and retries after three seconds. It
reports the problem once using its existing informational notice and stderr;
agent work is never stopped. Fix the desktop notification service/session bus;
Pico does not need restarting. Successfully delivered events are not replayed on
reconnection. Smoke mode never connects to the desktop notification service.

## Sidebar status and unchecked completion

Session rows have a subtle blue moving wave while the main agent or its children
are working. A pending questionnaire takes priority and freezes the wave in yellow;
otherwise unacknowledged errors/interruption (including child failures) freeze it in
red once work stops. Opening an error session or the existing Escape acknowledgement
clears error attention; questions remain yellow until answered or cancelled.

Successful main-turn completion replaces the row's age label with a blue circle
until that conversation is opened again. Opening it clears and saves the flag,
regardless of keyboard focus; opening a child does not check its parent. Refreshing
or focusing an already-open conversation does not clear the dot. New work also
clears it. Stop, errors, pending questions and turns waiting on children do not
create completion circles.

The flag lives in session metadata and sidebar summaries, independent of desktop
notification delivery, dismissal and DND. Ordinary session saves (including eviction
and shutdown) persist it across restarts; it is a sidebar hint, not a separate
completion transaction or a guarantee against a crash before the next save.
Opening a session uses the existing edit/save path and its storage-error recovery.
Waves use widget-local redraws only for visible working rows, retain their displayed
phase when paused, and use a wgpu triangle mesh with shared theme colors.

## Verification

Run inside `nix develop`:

```sh
cargo test --locked -p pico-core agent::notifications
cargo test --locked -p pico-core agent::subagents
cargo test --locked -p pico-evo notifications
just verify
```

The protocol test starts its own private `dbus-daemon` and fake notification
server. It checks payload privacy, desktop identity, replacement IDs, action
routing, withdrawal and dismissal deduplication without touching the real desktop
bus or user data. Nix provides `dbus-daemon` for development and package tests;
it is **not** an installed runtime dependency or an app-launched service.

Manual desktop acceptance (use isolated Pico state):

1. Run a main turn while focused on it: no desktop notification.
2. Switch to another conversation, then finish the first: its title and
   **Agent finished** appear. Click it and verify session selection/draft retention.
3. Unfocus Pico with that conversation selected and repeat: it notifies.
4. Ask several questions in one questionnaire: one **Answer needed**; answer or
   stop it and verify withdrawal. Queue another questionnaire and verify it alerts
   only when it becomes actionable.
5. Keep a child running while the parent pauses: no completion alert until the
   parent finishes processing the children's results.
6. Cause a terminal provider/auth failure: one generic error notification, with
   details only in Pico. Stop a running turn: no error/completion notification.
7. Dismiss a notification; refresh/switch/restart Pico: no historical replay.
8. Complete a background session: its blue sidebar circle replaces the age. Close
   Pico normally and restart before opening it: the circle remains. Open it: the
   age returns and remains after another restart. Focusing an already-open completed
   conversation does not clear its dot; opening its child does not clear it either.
9. Observe working, questionnaire and error rows: blue waves move; yellow/red waves
   stay frozen, including during other animations. Answering resumes the wave.
   Scroll/reorder/filter rows and check selection, hover and context-menu targets.
10. Enable desktop DND and verify Pico does not bypass it. Where safe, stop/restart
   the notification service and verify automatic recovery, without interrupting
   an agent or repeating previously delivered notifications.
