# Fonts

The game loads three faces at startup, from this directory:

| File | Role |
| --- | --- |
| `NotoSans-Regular.ttf` | body text, HUD, Cyrillic |
| `Fantique-Snails.ttf` | decorative display face (titles) |
| `Dragon-Serial-Regular.ttf` | decorative serif face (headings) |

## Provenance

`Fantique-Snails.ttf` and `Dragon-Serial-Regular.ttf` come from the Deenote
Deemo asset set (the asset bundle used by the Deenote project for Deemo-style
charts). They are kept here because this is a fan project that recreates the
look of the original game and the UI is designed around these two faces.

`NotoSans-Regular.ttf` is Noto Sans Regular, released under the SIL Open Font
License 1.1 (OFL).

## Licensing, stated plainly

* The Unlicense in the repository root covers the **code only**. It does not
  cover these font files.
* Noto Sans is redistributed under the OFL.
* No license is claimed for the two decorative fonts. We did not author them
  and cannot relicense them. They are included as part of a non-commercial
  fan project, on the assumption that this is acceptable; if that assumption is
  wrong, they should be removed by whoever holds the rights.
* If you fork this repository for anything beyond personal use, treat the
  decorative fonts as replaceable: drop in your own `.ttf` files under the same
  names, or delete them.

## Missing fonts are not fatal

If `Fantique-Snails.ttf` or `Dragon-Serial-Regular.ttf` is absent, the game
prints a note and falls back to Noto Sans for that role, so a clone without the
decorative faces still starts and renders.

`NotoSans-Regular.ttf` itself is required: without it the renderer has no face
for body text and startup fails.

Japanese and Chinese titles additionally use a system CJK font when one is
installed (Noto Sans CJK is looked up in the usual font directories); without
one, those titles fall back to whatever glyphs the bundled faces provide.
