# Supported tablets

The game reads the pen directly from the tablet's raw HID device
(`/dev/hidraw*`), matched by USB vendor/product id — no vendor driver, no
libinput, no absolute-mouse emulation. Only the horizontal position and the
pressure are used; the vertical axis is ignored.

Tablet input is **early-stage**: the list below is short and will grow. If your
tablet is not listed, the pen simply does nothing today (the mouse still works).

| Tablet | USB id | X range | Pressure | Notes |
| --- | --- | --- | --- | --- |
| Gaomon S620 | `256c:006d` | `0..26455` | `0..8191` | 12-byte reports; hover = pressure `0` |
| Parblo Ninos N4 | `0483:a019` | `0..21597` | `0..4358` | 10-byte reports; hover/press flag in byte 1 (`0xA0` hover, `0xA1` press) |

Both are used as the **left cursor** (the mouse drives the right one), and both
can be connected at the same time — the game follows whichever one reported
last.

## Why there is no udev rule

The obvious fix — telling libinput to ignore the tablet
(`LIBINPUT_IGNORE_DEVICE=1` in a udev rule) — is the wrong one: it applies to the
whole system, so the tablet stops working in every other application until udev
is reloaded. Some tablet packages install such rules for their own virtual
devices; that is their business, not a game's.

This game reads the pen straight from the raw HID device, so it never needs the
system to hide anything. Instead it drops the *mouse* events the pen emulates:
a click that arrives within a few milliseconds of a pen press (or while the pen
is down) is ignored by the game, so the pen does not double-fire. Hovering does
not suppress the mouse, which keeps the two-cursor setup working.

Nothing to install, nothing to undo.

## Adding a tablet

1. Run the game once with the tablet connected — it writes
   `.work/tablet_ingame.jsonl` (one JSON line per raw report, with the hex
   payload).
2. Collect: the `lsusb` line for the tablet, 5–10 lines from that log while you
   move the pen and press it, and the tablet's active width (the raw X value at
   the right edge).
3. The mapping lives in `native/src/tablet.rs`: the vendor/product ids passed to
   `find_nodes`, the report parser in `spawn` (byte offsets for X and pressure),
   and an `*_X_MAX` constant for the active width. Add a case there and send it
   upstream.

## See also

- [README](README.md) — install, controls, adding charts.
