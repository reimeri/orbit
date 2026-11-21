# Lucide icons

Official SVGs from [Lucide](https://github.com/lucide-icons/lucide), release 0.468.0,
pinned to commit `f12b0de177fbc2a6795e99be065887e72b237123` (`icons/`).
See `LICENSE` for the upstream ISC notice, supplemented with the Feather MIT
notice from Lucide commit `86d323875837015ddf74d044df1f4c09e99c6885`.

Only the icons used by Orbit are bundled and embedded at compile time; there are
no runtime downloads. `star-filled.svg` is the upstream `star.svg` with
`fill="currentColor"` for the selected favorite state. `book-open.svg` and `map.svg`
use a lighter stroke to match the model glyphs. All other SVGs are unmodified.

Use the shared `crate::icons::Icon` component so SVGs inherit the enclosing
control's foreground color, including hover and disabled states. Standard inline
and menu icons are 16 px; compact disclosures are 12 px and rail icons are 24 px.
