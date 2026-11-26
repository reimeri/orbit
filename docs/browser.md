# Session browser

Open **Browser** (the globe in the right rail) in an existing session. Click the
address, enter an HTTP(S) URL or a development server such as `localhost:5173`,
and press Enter. Back, forward and stop/reload sit to the left of the address;
the driver menu, **⋯** (browser options) and **×** (hide panel) sit to its right.
On narrow panels the driver menu shows just the bot or person icon.

Use **⋯** to find text, zoom, expand or restore the panel, change logins, or
close the tab. With the page focused, Ctrl+L selects the address and Ctrl+F
replaces it with Find. Press Enter for the next match; dismiss Find with its
× or Escape to restore the address. Clipboard shortcuts exchange plain text
with the desktop.

Orbit starts the sandboxed CEF helper on first use, not on startup. The user
and the main agent drive **the same tab**. Switching panels or sessions hides
the page and stops its off-screen painting; it does not reload the document.
**⋯ → Close tab** discards the page. Pages are not restored after quitting
Orbit. Background tabs still consume memory and can run site JavaScript.

- **Project login** shares cookies and site storage between sessions of the
  same project, including its Git worktrees. Persistent cookies survive an
  Orbit restart subject to the site's expiry rules. This isn't a password
  manager, and Orbit doesn't import a desktop browser profile.
- To test a separate account, choose **⋯ → Login: project… → Use isolated login**. This replaces the current page with a blank
  in-memory browser context. Its site data disappears when the tab closes.
  The session remembers the isolation choice.
- The agent has control by default. **Agent drives → Take control** blocks its browser input
  and screenshots, including pending captures. Already dispatched actions
  cannot be undone. Only **You drive → Hand to agent** grants access again; the choice
  survives closing the tab and restarting Orbit. For sign-in, take control
  and type credentials directly into the page, not into chat or a tool call.
- Browser ownership applies to Orbit's browser tool. It does not sandbox the
  agent's existing shell or restrict other programs running as the same user.

The first version has one viewport per session. New-window links reuse that
viewport; sign-in flows that require a separate live opener/popup are not
supported. Native file dialogs, downloads, browser extensions, and rich
clipboard transfers are not part of this integration.

## Agent interface

Only a main conversation gets `tools.browser`. There is no tab ID, cookie
export, arbitrary page-script tool, or network debugging port. Coordinates
for input are relative to the last screenshot in **image pixels**, including
on fractionally scaled displays. Navigation returns when dispatched, not when
the web app has finished rendering. A screenshot wakes a hidden tab for a
fresh paint and hides it again.

```js
await tools.browser({action:"navigate", url:"http://localhost:5173"});
image(await tools.browser({action:"screenshot"}));
```

Use `click` with `x,y`, `type` with `text`, `key` for named keys, or `scroll`
with `x,y,dy` (negative is down). Back, forward, stop and reload are also
available. Agent tool calls and requested screenshots become session history;
manual page input is not recorded as tool calls. Treat whatever the page
shows as untrusted content.

## Packaging and checks

CEF and its API-matched helper are supplied by the pinned Nix package.
Re-enter `nix develop` for a new helper after changing `browser/`. Persistent
profiles live under `$XDG_CONFIG_HOME/orbit/browser/` (or
`~/.config/orbit/browser/`). Removing projects from the sidebar does not erase
shared site data. To clear **all** project sign-ins, quit Orbit and move that
browser directory aside. This does not remove conversations or saved tool
screenshots.

Run `nix develop --command just browser-smoke` for a real helper, disposable
profiles, a loopback site, code-mode screenshot delivery, and an iced
render/input test. Run as a regular user with Chromium's Linux sandbox
available; don't work around startup errors by disabling it.
