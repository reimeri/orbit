# Desktop notifications

Orbit sends native Linux desktop notifications for **main conversations**:

- **Answer needed** when a blocking or async questionnaire first reaches the front of its queue.
  Blocking questions take priority. Answering or stopping a blocking question withdraws
  its alert immediately; returning to a previously shown async question does not alert again.
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

Notifications are suppressed **only when Orbit has keyboard focus and that exact
conversation is selected**. Other conversations notify even while Orbit is focused;
all conversations can notify while it is unfocused. Suppressed events are not
replayed when focus changes. Viewing a child is not viewing its parent.

The summary contains the session title; the body contains only the generic reason
above. No question, response, tool output, filesystem path or error detail is
copied into the notification body. Desktop Do Not Disturb, expiration, history and
sound policy remain under the notification server's control. Orbit adds no settings,
notification history or notification controls to its GUI.

Clicking a notification selects its conversation, preserving drafts through the
normal serialized navigation path. When the server sends an
[`ActivationToken`](https://specifications.freedesktop.org/notification/latest/protocol.html)
before the matching `ActionInvoked` signal, Orbit submits it to the compositor for its existing Wayland
surface. **The compositor decides whether to raise or focus the window.** If
there is no token or `xdg_activation_v1` is unavailable, Orbit requests
informational attention instead. It does not force focus or open another window.
Deleted, answered, stopped and superseded notifications cannot navigate to stale
work. Desktops without notification action support still display the notification.

## Lifecycle and delivery

`orbit-core/src/agent/notifications.rs` keeps live, uniquely identified attention
signals separate from the coalesced UI refresh stream. Events remain available
until consumed, with superseded events invalidated. Terminal events survive
in-process transcript eviction but are not persisted or replayed across restarts.
Notification delivery is independent of the existing error acknowledgement state.

`orbit-app/src/notifications.rs` applies focus/selection policy and delivers via
`org.freedesktop.Notifications` on the session bus, identifying the desktop entry
as `orbit`. One notification per conversation is replaced as its reason changes.
Answered/cancelled questions, resumed/deleted sessions and conversations brought
into focus are withdrawn where the desktop supports it. Dismissing or expiring a
notification does not cause repeated delivery.

D-Bus calls run asynchronously with a five-second timeout. If delivery fails, Orbit
retains the latest still-actionable events and retries after three seconds. It
reports the problem once using its existing informational notice and stderr;
agent work is never stopped. Fix the desktop notification service/session bus;
Orbit does not need restarting. Successfully delivered events are not replayed on
reconnection. Smoke mode never connects to the desktop notification service.

### Native window activation

Upstream winit 0.30.13 can consume startup tokens when creating a window, but
not after creation. Three checksum-pinned native-layer patches bridge the gap:
[iced runtime](../patches/iced-runtime/notification-activation.patch) carries
the token and window ID in a task; [iced-winit](../patches/iced-winit/notification-activation.patch)
resolves that ID to a live window; [winit](../patches/winit/notification-activation.patch)
uses its already-bound `xdg_activation_v1` and `wl_surface`. Closing the window
before dispatch cancels the request. No raw handle, second Wayland connection or
process-wide startup environment variable is needed.

## Sidebar status and unchecked completion

The sidebar lists **Needs attention** (pending questions and errors) first, then
**Working**, then **Sessions** (including unchecked completions). Ordering within
each section is unchanged. Enable **Hide working sessions** in General settings to
move Working into a collapsible section above Deferred and Settled. Search expands
matching sections without changing their normal expansion state.

Session rows have a subtle blue moving wave while the main agent or its children
are working. A pending questionnaire takes priority and freezes the wave in yellow;
otherwise unacknowledged errors/interruption (including child failures) freeze it in
red once work stops. Opening an error session or the existing Escape acknowledgement
clears error attention; questions remain yellow until answered or cancelled.

Successful main-turn completion replaces the row's age label with a blue circle
until that conversation is viewed. The selected conversation's flag clears and
is saved when the window gains keyboard focus, or immediately on completion if
the window is already focused. Opening a conversation also clears and saves its
flag regardless of keyboard focus. Background conversations retain their dots;
opening or focusing a child does not check its parent. New work also clears it.
Stop, errors, pending questions and turns waiting on children do not create
completion circles.

The flag lives in session metadata and sidebar summaries, independent of desktop
notification delivery, dismissal and DND. Ordinary session saves (including eviction
and shutdown) persist it across restarts; it is a sidebar hint, not a separate
completion transaction or a guarantee against a crash before the next save.
Both opening and focus-based acknowledgement use the existing edit/save path
and its storage-error recovery.
Waves use widget-local redraws only for visible working rows, retain their displayed
phase when paused, and use a wgpu triangle mesh with shared theme colors.

## Verification

Run inside `nix develop`:

```sh
cargo test --locked -p orbit-core agent::notifications
cargo test --locked -p orbit-core agent::subagents
cargo test --locked -p orbit notifications
just verify
```

The protocol test starts its own private `dbus-daemon` and fake notification
server. It checks payload privacy, desktop identity, replacement IDs, action
routing with interleaved tokens, late signals for replaced or dismissed IDs,
withdrawal and dismissal deduplication without touching the real desktop
bus or user data. Nix provides `dbus-daemon` for development and package tests;
it is **not** an installed runtime dependency or an app-launched service.

To check desktop integration, use isolated state. Finish a turn outside the
focused session, focus another application, then click **Open conversation**:
Orbit should select the right session without losing its draft and, where the
compositor permits activation, bring its existing window forward. To distinguish
activation from an attention request, run the isolated instance with
`WAYLAND_DEBUG=client` and look for `xdg_activation_v1.activate` carrying the
server's token for its existing surface. An `activate` request alone is not
proof: winit's attention fallback requests its own token. Treat traces as
private. Repeat with DND enabled to check desktop policy. The private-bus test
does not verify compositor behavior or window raising.
