# Local skills

Skills are workflows an agent can choose when relevant. Unlike `AGENTS.md`,
a skill's Markdown instructions are not automatically included in every chat.

## Where to put a skill

Create `<root>/<skill-folder>/SKILL.md`. Pico looks in these roots:

- **Repo:** `.agents/skills/` in the selected project folder and each ancestor
  up to and including its nearest Git root. Outside Git, only the selected
  folder is checked. Opening a repository root does not activate its subprojects' skills.
- **Personal:** `~/.agents/skills/`, across all projects.

For example, this repository has
[`update-openai-models`](../.agents/skills/update-openai-models/SKILL.md).
A minimal skill looks like this:

```markdown
---
name: review-migration
description: Review a database schema migration before it ships.
---

Read the proposed migration and check whether existing rows remain valid.
```

Use lowercase letters, digits and hyphens for the name. Names can repeat: the
picker shows each skill's scope and file path. Symlinked skill folders are
supported. Pico reads local skills; it does not install packages, register new
tools or execute anything during discovery.

## How to use a skill

- **Implicit:** describe the task. Main agents and read-only subagents receive
  a developer message listing names, descriptions and paths, separate from the
  base prompt. An agent can open a matching skill and its references as needed.
  Setting `policy.allow_implicit_invocation: false` in the skill's
  `agents/openai.yaml` keeps it out of that catalog without hiding it from the picker.
- **Explicit:** type `$` at the beginning of a word in the composer, filter by
  name, then click a skill or select it with Up/Down and Tab/Enter. Escape
  dismisses the picker. The selection inserts an editable Markdown link to the
  exact skill. Pico captures the selected file when you **send**, including
  steering messages, and attaches its contents as user context, not developer
  instructions. A missing or renamed selection blocks the send and leaves the
  draft intact; reselect the skill or remove the link. A bare name is not a
  captured selection.
- The picker rediscovers on opening or refresh. Starting a turn rediscovers
  the catalog. Changes append a new catalog to the conversation rather than
  editing earlier messages. Retrying an accepted submission uses the captured
  body, not a changed file. Resending a fork draft makes a new selection at send time.

Skills do not grant permissions or change a subagent's role. Only use skills
you trust: Pico's existing shell tools run with your user privileges.
