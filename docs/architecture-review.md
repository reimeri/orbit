# Part 3 review: changes and decisions

The aim was to remove competing sources of truth and repeated work, not to
reduce line counts by dropping features.

| Finding | Decision |
| --- | --- |
| App, message router and right panels | Replace six independent visibility fields with one `Option<RightPanel>`. Rendering matches on the enum; transitions handle Settings drafts, Changes cleanup, popup dismissal and layout invalidation. Worktrees retains its own in-progress operation guard. Move composer and transcript logic out of the root module. Keep a root update for cross-domain event ordering. The task queue batches effects from nested updates; storage itself already schedules by resource lane. Removing the queue or loosening UI admission requires a separate event-ordering change, not just moving code. |
| Settings copies | Keep the editable draft, submitted snapshot and last committed catalog distinct. They can *legitimately disagree*: Cancel can race a delayed Save reply, and a live layout autosave must not publish staged preferences. Keep the merge of immediate controls explicit. |
| Conversation and subagents | Move stop and session/project deletion into the lifecycle module; keep family membership and run indexing encapsulated. Move inline core agent tests next to their modules. Do not replace every flag/lock with one turn mutex: a new turn can start while the old generation is finishing, and naming, resident data, background cells and durable assignment delivery have independent lifetimes. Keep the generation-scoped cleanup and family admission from the preceding fixes. A future turn-state change needs to model that overlap and retain cheap UI reads. |
| Filename index | When Git supplies the files, watch discovery still needs directories, including empty ones and parents of tracked-but-ignored files. When it does not, collect files and watch directories in one ignore-aware traversal. Retain the compact indexes, bounded cancellable sort, private cache, watch budgets and descriptor-relative scoped walker: they provide large-workspace performance, cancellation and symlink containment. Only Git metadata and Orbit’s own state and cache have [special exclusions](file-tags.md#index-and-lifecycle). Other names follow normal workspace ignore rules. |
| Widgets, virtualization and geometry | No general-purpose replacement. Transcript rows, a Markdown subtree, wrapped tool text and native editor rows have different identities, measurement lifetimes and scroll anchors. Selection and find need geometry from the rendered frame; hiding an image source during an overlay transition and tracking native editor focus are not additional virtual lists. Exact viewport matching in Find distinguishes nested anonymous scrollables and rejects stale geometry. An epsilon match could scroll the wrong one. Revisit with a reproducible mis-target or an upstream stable scrollable ID. See [rendering and find](performance.md#rendering). |
| Upstream patches | Keep the pins and zero-fuzz application. The patch families cover native Wayland clipboard/activation, screenshot ordering, editor input/shaping/selection, Markdown, rendering and QuickJS lifetimes. Even the small menu and editor patches have visible interaction contracts. Removing a diff without an upstream equivalent would remove that behavior. Add the missing [iced-runtime rationale](../patches/iced-runtime/README.md); reevaluate the individual diffs when updating their respective upstreams. |
| Production smoke, marketing and fixtures | Default builds keep a small real-window smoke check. The staged suite, screenshot-ordering fixture, marketing flag and scene assets are opt-in via `staged-capture` (also available to unit tests). The installed-wrapper check still exercises the actual default binary; `just verify` runs the full staged sequence. Keep test/benchmark modules and their counters under `cfg(test)`: their location under `src` does not compile them into the shipped binary. |
| Repository clutter | Removed the obsolete offline migration utility and its tests. Leave local user files and Nix build results alone. |

## Browser and repeated core logic

- **Browser wire:** UI input, navigation, focus and viewport changes now use
  `orbit_core::browser::UserCommand`. Core supplies the tab identity and owns
  serialization. The host already duplicates the protocol descriptor and
  redirects stdout diagnostics; incoming tab events were already typed. Retain
  CEF event flags, virtual keycodes and the fixed frame layout as native ABI
  contracts. No schema generator or replacement transport was introduced.
- **Evaluation and spill:** retain the file-thread traversal, UI-thread lease
  checks and pre-opened private output file. Result size is unknown before
  running page code; losing the result or rerunning the expression if disk
  allocation fails afterward is worse than opening a small file. Browser
  thresholds are **bytes** in the wire protocol. Shell, cell and nested text
  results share the default **token/character** budget, not that byte limit.
- **Text and titles:** share assistant final-text extraction across naming,
  summarization and turn results. Transcript reconciliation also uses typed text
  parts but keeps intermediate commentary and reasoning. Session, fork and
  assignment excerpts use the first nonblank line; message and assignment
  length limits stay distinct.
- **File access:** skills and media share the nonblocking regular-file open.
  File tags still need a directory descriptor and `openat2` containment.
  Snapshot copying/version checking is separate from tag selection and reuse.
- **Auth and retry:** the token dirty flag was already moved under the credential
  mutex. Keep web and image dispatch separate: one can use a separate API key
  and incur a charge; cancellation diagnostics and body limits differ. Their
  synchronized credential refresh and error redaction are already shared.
  Likewise, retrying an HTTP/WS rejection before output and recovering an
  interrupted agent step after output have different replay guarantees.
- **History and saves:** keep plaintext provider-input reconstruction separate
  from the compactor's JSONL archive. The latter preserves metadata/references
  without sending UI-only backend data. The persisted changes for submissions,
  model reconciliation and narrow edits have different commit and publication
  rules; a generic snapshot replacement could overwrite streaming changes.
  Repeated checks for pending input at turn boundaries are now one predicate.
  This is deliberately distinct from checking active processes or unanswered
  questions.
- **Validation and long functions:** validate typed web operations directly
  instead of serializing them back to JSON for validation. GIF acceptance and
  playback share the frame limit without forcing validation to retain every
  decoded frame. Beyond the file-capture extraction, no wholesale split of
  QuickJS cell dispatch or shell startup: those paths coordinate cancellation,
  observers and ownership of yielded work, and moving lines alone does not
  simplify that contract.
