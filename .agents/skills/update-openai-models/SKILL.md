---
name: update-openai-models
description: Check official OpenAI documentation for newer Astra, Sol, and Luna models, then update Pico V2's fixed model catalog and supported reasoning efforts.
---

# Update the OpenAI model catalog

Use this skill when asked to refresh Pico's OpenAI models or check for newer
Astra, Sol, or Luna versions. This is a repository maintenance workflow, not a
runtime model-discovery feature. Read `PROJECT.md` and `docs/development.md` first.

## Research before editing

1. Use web search and read official OpenAI model pages, reasoning guidance,
   changelog, and (where documented) ChatGPT/Codex availability:
   - https://developers.openai.com/api/docs/models
   - https://developers.openai.com/api/docs/guides/reasoning
   - https://developers.openai.com/api/docs/changelog
2. Find the newest generally available **Astra**, **Sol**, and **Luna** family
   versions. Do not infer API identifiers or capabilities from marketing names,
   unrelated families, dates, or search snippets. Do not substitute a preview or
   specialist variant without user approval. API availability alone does not
   prove access through Pico's ChatGPT subscription endpoint.
3. Record exact API IDs, display names, supported `reasoning.effort` values,
   usable input context windows, tool support, and priority-tier limitations.
   Include source URLs and the check date in your report. If authoritative
   information is unavailable or conflicting, ask the user rather than guessing
   or changing a known-working catalog. No live API calls without authorization.

Current capability reference links (catalog introduced 2026-11-14):
- https://developers.openai.com/api/docs/models/gpt-6-astra
- https://developers.openai.com/api/docs/models/gpt-6.1-sol
- https://developers.openai.com/api/docs/models/gpt-6-luna

## Update the app

- `crates/pico-core/src/models.rs` is the single fixed catalog. Keep **one current
  entry per family**; replace older family versions rather than accumulating
  old versions or compatibility aliases. Preserve the default family (currently
  Sol), updating `DEFAULT_MODEL` to its replacement ID when needed.
- Keep effort lists model-specific. The current catalog has low, medium, high,
  xhigh, max for Astra/Sol; Luna additionally has none. Do not carry these lists
  forward without checking the replacement models' documentation.
- `crates/pico-core/src/store.rs` owns per-model preferences (Medium, Fast off
  initially). Preferences for unchanged IDs must survive catalog refresh.
  Newly introduced IDs receive defaults; retired preferences are removed. A
  retired app default resolves to `DEFAULT_MODEL`. Validate changed capabilities
  and decide explicitly how affected saved preferences should be corrected.
- `crates/pico-app/src/ui.rs` uses the same catalog for Models settings, the
  default-model dropdown, and footer choices. Do not add another hard-coded
  model list. Removed session models must be replaced by the configured app
  default, with that model's effort/Fast defaults, persisted, and accompanied by
  an informational toast. Existing valid session selections stay unchanged.
- There is currently **no custom-model editing**. Do not add it, remove other
  families, or read/modify user configuration or credentials as part of this
  skill. If the app later adds custom models, preserve them during refresh.
- `crates/pico-core/src/provider.rs` uses ChatGPT/Codex Responses. Fast currently
  sends `service_tier: "priority"` and
  `x-codex-routing-hint: model=<id>;tier=priority`. Normal mode omits the body tier
  and sends only `model=<id>` in the hint. Preserve body/header consistency on
  retries and compaction; do not invent additional headers.
- The context limit is currently user-configured, not inferred per model. Do not
  silently change users' limits based on a model's advertised total window.
- Update this skill's reference links, README, acceptance checklist, and affected
  tests. No V1 compatibility layer; all state remains under `pico-evo`.

## Verify and report

Use `nix develop --command ...` for tooling. Keep `target/` and existing build
flags. Run focused core/UI tests and `just verify`; run `just smoke-gpu` for UI
changes when a Wayland desktop is available. Cover any changed effort boundary,
preserved model preferences, retired-session recovery and notices, and request
body/header consistency. Avoid redundant catalog snapshot tests.

Report replacements, evidence, checks run, and any unverified subscription
availability. If there are no verified newer models, report that result without
making speculative edits. Never modify `.pico/media/` or the V1 reference.
