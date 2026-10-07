# keebs

**Linux-only, by design.** Rust port of the SteamlessKeyboard **Linux app** —
uinput injection, hidraw/libusb device access and X11/XWayland windowing
do not exist on Windows, and never will here. The helper trees' Windows
code is reference material only.

## Running

From inside the nix flake (LD_LIBRARY_PATH for SDL3 comes from the wrapper):

```sh
nix run .#keebs
```

Or hack on it in the keebs dev shell:

```sh
nix develop .#keebs -c cargo run --release
```

Starts hidden in the background, listening for the controller.
**Steam + X** opens the on-screen keyboard (Steam-dark theme, Selawik
font); it also listens for the mouse in case you want to click-type:

| input | action |
|-------|--------|
| **Steam + X** | open the OSK |
| **B** / **L4/L5 grip** / Steam released alone | close the OSK |
| **left stick / D-pad** | move the key cursor (tap = 1 key, hold = repeat) |
| **A** | press the key under the cursor |
| **X** | Backspace (hold to repeat) |
| **Y / R4 / R5** | Space |
| **L2** | Shift (hold) |
| **R2** | Enter (hold) |
| **trackpad touch** | move the pointer; **pad click** / L2/R2 presses the key under it |
| **right stick** | move the system mouse (point-and-click keys) |
| **Steam + left stick** | volume / prev-next track |
| **Steam + VIEW** | Alt+Tab |
| **L3** | Caps Lock (Steam+L3: play/pause) |

Mouse also works on the OSK directly: hover highlights, left-click presses,
right-click = Shift, back/forward buttons = Backspace/Space.

The bottom row has sticky **Ctrl**, **Alt**, **AltGr** (Right Alt — Polish
ą/ę/ś… on the programmer layout) and **Win** keys (click once — the real
modifier is held and the key highlights — then tap keys; click again or
close the keyboard to release). Sticky modifiers are dropped automatically
whenever Enter is sent, so Enter never arrives as Shift/Ctrl/Alt/Win+Enter.

### Live tuning (environment variables)

| Variable | Default | What |
|---|---|---|
| `KEEBS_PAD_GAIN` | `1.1` | pad→pointer gain — raise if the pads don't reach the window edges, lower if they feel twitchy (coordinates are clamped to the window, so overshoot is safe) |
| `KEEBS_SC_TICK` | `550,-6,5,18` | SC 2026 UI tick: `freq,gain,count,ms` (gain −127..127, higher = louder) |
| `KEEBS_SC_PADCLICK` | `550,-5,6,18` | SC 2026 pad-click buzz, same format |

Example: `KEEBS_PAD_GAIN=3 KEEBS_SC_TICK=800,-3,3,12 nix run .#keebs`
(The Deck's haptics are firmware-native and are never overridden.)

**Multiple controllers:** every attached device (Deck / SC 2026 / SC 2015)
gets its own reader; the OSK binds to whichever controller you actually
touch (300 ms hysteresis — a device in use can't be stolen mid-chord), and
switches automatically when you pick up the other one. Inactive devices
stay in desktop posture (lizard on).

`--listener` runs the phase-1 passive device listener instead (prints button
edges + battery, LGRIP1/L4 -> Left Shift). Detects the Steam Deck, the
Steam Controller 2026 (Triton), and the classic Steam Controller (2015,
0x1102 dongle / 0x1142 wired) automatically.

## Status

**Ported:** Triton (0x1304/0x1302) + Deck (0x1205) + classic Steam Controller
(0x1102/0x1142) drivers, lizard control, haptics/rumble/chime (Triton/Deck),
battery, uinput injection, EventMapper, chord logic (`controller.py` core),
on-screen keyboard (`vkb.py` layout/geometry/cursor stepping/dispatch,
`screen.py` rendering — SDL3 + SDL3_ttf via a libloading FFI that mirrors
the Python `sdl3w` shim).

### Classic Steam Controller (2015) — provisional wire tables

The classic driver's byte layout and button masks (see `parse_classic` in
`src/sc.rs`) are assembled from the ynsta/steamcontroller + SDL classic
structs and are **not yet calibrated against live hardware**. Haptics are
no-ops pending the 0x8F "singer" report, and battery is not parsed. To
calibrate against your controller:

```sh
nix develop .#keebs -c cargo run -- --probe-sc
```

Follow the prompts (hold each input for ~2.5 s); the probe prints which
bits/offsets moved per input. Paste the output into an issue and the
tables get fixed. The 2015 hardware has no sticks/D-pad — pads drive the
OSK (the original Steam behavior), so stick-cursor and Steam+stick media
chords are unavailable on it.

**Not ported yet:** skins/CSS theming + transparency + live tray skin
switch, open animation, click-through + window position rotation,
 stick-scroll mode, per-controller settings, gamepad mode, tray icon,
Windows tree.

## Design notes kept from the Python original

- **Lizard persistence trap (Triton)**: runtime lizard writes can stick in
  firmware; only a power-cycle restores defaults. The driver never touches
  lizard at runtime on Linux — the watchdog thread exists purely for
  teardown parity.
- **Self-expiring rumble**: motor tones are bounded bursts re-armed by a
  keepalive thread, so a crash can never leave a motor buzzing.
- **Deck stick auto-centering**: first ~30 quiet frames are averaged; raw
  rest offsets up to ~900 were observed on hardware.
- **Last-good-interface memory**: reconnects skip the dongle's silent
  slots, cutting mode-switch lag from ~1.5 s to milliseconds.
