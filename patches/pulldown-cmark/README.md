# Block TeX for chat Markdown

The stock math option parses inline dollars and does not recognize backslash
brackets. This opt-in block rule recognizes a line starting with `\[` or `$$`
and ending (possibly on a later line) with the matching unescaped closer.
It runs alongside fenced code in the block parser: inline Markdown cannot
strip TeX escapes, fences and indented code stay code, and container prefixes
are handled by the same scanner as lists and quotes. Unfinished input remains
literal; a blank paragraph ends an unmatched block so later Markdown can recover.
The bounded lookahead is only for multiline blocks; the app separately limits
typesetting work without changing saved messages.

The pinned iced-widget display-math patch consumes the resulting DisplayMath
event using the original offset for incremental reparsing, with a raw-source
default viewer. No other cmark consumer needs to enable the option. On upgrade,
reapply against the checksum-pinned crate, re-enter the dev shell and run
the equation streaming/copy tests, `just verify`, and `nix flake check`.
