# Composer spell check

Orbit checks composer drafts locally with [Spellbook](https://docs.rs/spellbook/).
Spell check is on by default with US English. Open **Settings → General →
Spell Check** to turn it off or select another dictionary, then **Save**.
Misspellings are underlined; Orbit does not autocorrect, change the draft, or
send it anywhere for spelling analysis. Code, URLs, paths, common identifier
forms, and `@` file references are excluded. The word you are typing is not
marked until you move the caret or type a separator.

## Add a language

1. Get a matching Hunspell dictionary pair, for example `es_ES.aff` and
   `es_ES.dic`. Both files must be UTF-8.
2. Put the pair under `$XDG_DATA_HOME/orbit/dictionaries/` (normally
   `~/.local/share/orbit/dictionaries/`; create it if necessary).
3. Restart Orbit to discover the new files. In Spell Check settings, choose
   the language by its filename stem and save.

A user dictionary takes precedence over a packaged pair with the same stem.
If the selected pair is missing, the dropdown shows its name followed by
**(not found)**. Hover over the dropdown for details if Orbit cannot load the
active dictionary. The composer stays unmarked until a usable dictionary is selected.
There is no automatic language fallback or dictionary download.

## Packaging

The Nix shell and package supply the `en_US` pair and its license from
Nixpkgs. For other packaging layouts, set `ORBIT_DICTIONARY_DIR` to the absolute
path of the packaged dictionaries. No Enchant or Hunspell runtime is required.
