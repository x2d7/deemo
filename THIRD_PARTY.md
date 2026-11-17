# Third-party assets

The code in this repository is released into the public domain (see LICENSE).
The files listed here are **not** covered by that dedication. They are shipped
because the game loads them at startup.

## Fonts — `data/fonts/`

| File | What it is | Status |
| --- | --- | --- |
| `NotoSans-Regular.ttf` | Noto Sans Regular | SIL Open Font License 1.1 (OFL). Redistributable under the OFL, which also covers the copy bundled here. |
| `Fantique-Snails.ttf` | decorative display face used for titles | Not OFL, not public domain. Comes from the Deenote Deemo asset set. |
| `Dragon-Serial-Regular.ttf` | decorative serif face used for headings | Not OFL, not public domain. Comes from the Deenote Deemo asset set. |

See `data/fonts/README.md` for the details and the caveats.

## Sprites — `data/sprites/`

`data/sprites/stage/*.png` (stage background, judgement line, note and hold
sprites, note explosion frames, shockwave effects) are Deemo game art taken
from the Deemo asset set. They are included so the game can render the stage at
all; they are not covered by the Unlicense and they are not ours to license.

## Content the game downloads or imports

Charts, audio, covers and previews are never part of this repository. They are
either fetched from the chart server (see README) or imported by the user as
`.deemopack` files; all rights in that content stay with its authors.

## Crates

All Rust dependencies are regular crates.io releases; their licenses are listed
in `native/Cargo.lock` and by `cargo metadata`.
