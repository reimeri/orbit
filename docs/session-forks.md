# Session forks

Hover over a forkable user prompt or the controls row just below it. Click the
**fork icon** on the right to start a new session. Nothing is sent automatically.
The local weekday and send time appear beside the icon.

Text and images start in the new session's composer. Keep an original file tag to reuse its
captured bytes, remove it to omit the file, or add a new tag to attach another
workspace file.

Use the **Forks** button on the right rail to see related conversations. Clicking
the fork count beneath a prompt opens the same tree and highlights the branches
that started at that prompt. The panel stays open as you switch within the family
and closes when you leave it.

In a fork, **Forked here** separates inherited history from the new conversation.
Choose **View parent** on that separator to jump to the original prompt. Forks remain ordinary sidebar sessions: rename,
settle and delete them individually. Deleting a project still deletes its sessions.
Deleting the source conversation does not delete its forks.

Forks share the working directory; starting a fork does **not** roll back files,
re-run commands or inherit live tools or subagents. Media directories used by
inherited context are retained until the last referencing session is deleted.

## Exact boundaries

Only main-conversation prompts submitted at a stable turn boundary have
fork points. Steering submissions do not. While a turn is running you can still fork any saved point.

Each point records the pre-prompt transcript, ordered provider input, account and
compaction state in the acceptance transaction. Comparing successive immutable
histories keeps ordinary SQLite writes proportional to changed rows; replacing
context during compaction does not erase earlier points. Fork creation writes
the selected prefix and earlier points into a standalone session, with a fresh
runtime and no pending submissions. An open session retains its checkpoint
history in memory, and creating a fork incurs storage proportional to the
inherited history.
