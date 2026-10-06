# keebs

Rust port of the SteamlessKeyboard **Linux app**.

## Running

From inside the nix flake (LD_LIBRARY_PATH for SDL3 comes from the wrapper):

```sh
nix run .#keebs
```

Or hack on it in the keebs dev shell:

```sh
nix develop .#keebs -c cargo run --release
```

Opens the on-screen keyboard (Steam-dark theme, Selawik font). It listens
for the controller in the background:

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

`--listener` runs the phase-1 passive device listener instead (prints button
edges + battery, LGRIP1/L4 -> Left Shift).

## Status

**Ported:** Triton (0x1304/0x1302) + Deck (0x1205) drivers, lizard control,
haptics/rumble/chime, battery, uinput injection, EventMapper, chord logic
(`controller.py` core), on-screen keyboard (`vkb.py` layout/geometry/cursor
stepping/dispatch, `screen.py` rendering — SDL3 + SDL3_ttf via a libloading
FFI that mirrors the Python `sdl3w` shim).

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
