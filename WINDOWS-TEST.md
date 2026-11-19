# Windows test build

This build is for checking the native Windows port. The Linux build must not be
affected; nothing here is a release.

## 1. Unpack and run the probe

Unpack the whole zip into a normal writable folder (Desktop, `C:\deemo`, not
`Program Files`). Open **Command Prompt** in that folder and run:

```
deemo.exe --tablet-probe 15
```

If the command prompt returns immediately (the game is a GUI app), use
`start /wait deemo.exe --tablet-probe 15` and wait for it to finish.

No game window should open. During those 15 seconds:

1. move and tap the pen on the **Gaomon** tablet for about 5 seconds;
2. then move and tap the pen on the **Parblo** tablet for about 5 seconds.

The probe samples up to 20 moving/hover reports per HID interface, spread over
the whole 15 seconds, plus pressed reports even after that quota. Both tablets
must be used while it runs. A `tablet_probe.log` file appears next to
`deemo.exe` (the log is authoritative even if the console stays empty).

## 2. Send the log

Send `tablet_probe.log` back. It contains:

* every HID interface with path, VID/PID, interface number, usage page/usage,
  product/manufacturer/serial strings, report descriptor in hex, and the
  descriptor-derived input report length;
* up to 20 spread samples plus pressed samples (at most 40 per interface) with
  millisecond timestamps, plus the parsed X/pressure when the parser matches;
* a summary with the number of reports and the observed X/pressure range.

Also send `deemo.log` from the game run in step 3: it has the tablet startup
lines (which interfaces were opened, and whether the vendor or digitizer path
became active). If `tablet_probe.log` is missing, `deemo.log` is the fallback.

## 3. Run the game

```
deemo.exe
```

The game should open straight into the library — there must be **no black
console window** behind it. Then:

* move the pen over each tablet: the left cursor should follow the pen;
* tap notes: the press must register (same as the mouse hand);
* if both tablets are connected, test them one after another and then both at
  once;
* check music playback and the fps counter in the corner.

`deemo.log` is rewritten on every run and contains tablet startup lines such as
`tablet: gaomon 256c:006d opened (...)` or
`tablet: none found (hidapi on windows)`.

## 4. What to report

* Windows version and whether the tablets are connected over USB.
* Which tablet was moved first during the probe, and whether it appears in
  `tablet_probe.log` at all (device names, usage pages, number of interfaces,
  raw report lengths).
* In the game: does the left cursor move, do presses register, does one pen
  work at the same time as the other? What does `deemo.log` say about the
  tablets (`opened`, `vendor layout active`, `digitizer interface selected`,
  `none found`)?
* Do Japanese/Chinese pack titles render as text (not blank squares or empty
  lines)?
* Audio: works / silent / crackles.
* Approximate fps and whether it stays stable.
* Any crash, freeze, missing window, black console, or `deemo.log` errors.
