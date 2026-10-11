# keebs

## Credits

**Huge credits to [SteamlessKeyboard](https://github.com/PietPetGit/SteamlessKeyboard)**
by [PietPetGit](https://github.com/PietPetGit) — keebs is a Rust port of its
Linux app (controller drivers, lizard logic, chord handling and the
on-screen keyboard all trace back to that Python codebase). None of this
would exist without it. Go star the original.

**Linux-only, by design.** Rust port of the SteamlessKeyboard **Linux app** —
uinput injection, hidraw/libusb device access and Wayland windowing
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

## Install (NixOS flake)

```nix
# flake.nix
inputs.keebs.url = "github:tajo48/keebs";

# a NixOS module (package on PATH + udev uaccess rules + GNOME autostart)
environment.systemPackages = [ inputs.keebs.packages.${pkgs.system}.keebs ];
```

The binary needs write access to `/dev/uinput` and the Valve hidraw devices
(`ATTRS{idVendor}=="28de"`) — grant them to the active seat with
`TAG+="uaccess"` udev rules. Licensed under the MIT license (see LICENSE).

Starts hidden in the background, listening for the controller.
**Steam + X** opens the on-screen keyboard (Steam-dark theme, Selawik
font); it also listens for the mouse in case you want to click-type:

| input | action |
|-------|--------|
| **Steam** (tap) | toggle **gamepad mode** on/off |
| **Steam + X** | open the OSK (also leaves gamepad mode) |
| **B** | close the OSK |
| **R5** (right lower paddle) | toggle: open when closed, close when open |
| **left stick / D-pad** | move the key cursor (tap = 1 key, hold = repeat) |
| **A** | press the key under the cursor |
| **X** | Backspace (hold to repeat) |
| **Y** | Space |
| **L4** (left upper paddle) | hold **Ctrl** |
| **L5** (left lower paddle) | hold **AltGr** (Polish ą/ę/ś…) |
| **R4** (right upper paddle) | hold **Shift** |
| **trackpad touch** | move the pointer; **pad click** / L2/R2 while touched presses the key under it |
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
| `KEEBS_SC_TICK` | `550,-6,5,18` | SC 2026 UI tick (cursor moves): `freq,gain,count,ms` (gain −127..127, higher = louder) |
| `KEEBS_OSK_SCALE` | `0.8` | OSK window scale (1.0 = original 1286×369; fonts/layout scale with it) |
| `KEEBS_OSK_ALPHA` | `0.5` | OSK transparency for background + key fills (0.10..1.0; labels stay opaque). Native on Wayland (ARGB window) |
| `KEEBS_GAMEPAD_VID` / `KEEBS_GAMEPAD_PID` | `28de` / `11ff` | virtual gamepad USB ids (hex, no `0x`) — default is Steam Input's own output identity so Steam doesn't grab it (see below) |

Example: `KEEBS_PAD_GAIN=3 KEEBS_SC_TICK=800,-3,3,12 nix run .#keebs`
(The Deck's haptics are firmware-native and are never overridden.)

**Multiple controllers:** every attached device (Deck / SC 2026 / SC 2015)
gets its own reader; the OSK binds to whichever controller you actually
touch (300 ms hysteresis — a device in use can't be stolen mid-chord), and
switches automatically when you pick up the other one. Inactive devices
stay in desktop posture (lizard on).

## Gamepad mode

Tap **Steam** (alone, no game detection): the controller becomes a virtual
Xbox 360 gamepad via `/dev/uinput` and the keyboard is fully disabled (OSK
hides, no key/mouse injection, firmware lizard off — the pad is the only
output a game sees). Tap **Steam** again to leave; **Steam + X** leaves and
opens the OSK. A haptic tick confirms every switch.

| controller | virtual pad |
|---|---|
| A / B / X / Y | A / B / X / Y |
| LB / RB | LB / RB |
| LT / RT | analog triggers + digital button view |
| VIEW / START | Back / Start |
| L3 / R3 | stick clicks |
| D-pad | D-pad |
| left/right stick | left/right stick |
| pads (classic 2015 only) | left/right stick (the classic has no sticks) |

The guide button is **never** emitted — Steam reacts to it by opening Big
Picture / the overlay.

### Why Steam leaves it alone

The pad identifies itself as **`28de:11ff` "Steam Virtual Gamepad"** — the
exact vendor/product/name of Steam Input's *own* uinput outputs. Steam's
controller manager must skip its own outputs (otherwise it would recursively
grab them), so running Steam does not turn the pad into a Steam Input
controller — which is the Linux port of "Block SteamInput Xbox Controller
grab". Games still see it normally: SDL2/SDL3 explicitly enumerate
`28de:11ff` devices as gamepads (with the standard Xbox button order
auto-mapped), and Proton reads the evdev node directly.

If a setup ever needs a neutral identity, override it
(`KEEBS_GAMEPAD_VID`/`KEEBS_GAMEPAD_PID`, hex) — but note that e.g. the
genuine Xbox ids `045e:028e` WILL be grabbed by Steam. The env-var route
(`SDL_GAMECONTROLLER_IGNORE_DEVICES=<vid>:<pid>`) hides a device from a
whole process tree, so games launched from that Steam would lose it too —
prefer the default identity.

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

**Not ported yet:** skins/CSS theming + live tray skin
switch, open animation, click-through + window position rotation,
 stick-scroll mode, per-controller settings, tray icon,
Windows tree.

## Design notes

- **Pure Wayland, no X11 fallback**: the OSK is a native Wayland window
  (SDL3 wayland driver, chosen by default under `WAYLAND_DISPLAY`). The
  whole X11 layer — WM hints, override-redirect, XRaiseWindow, the
  xwayland-satellite focus guard — is gone. What Wayland cannot give a
  regular toplevel is deliberately not implemented:
  - **no always-on-top / restacking** (compositor owns stacking; keep the
    OSK accessible via your compositor's floating rules),
  - **no absolute positioning / programmatic moves** (place it with
    compositor window rules — see below),
  - **no global modifier queries** (Caps Lock state comes from the sysfs
    capslock LEDs only).
  The window is created unfocusable (`SDL_SetWindowFocusable(false)`) so
  the uinput-injected keys keep landing in the app behind the OSK, while
  pointer input (click-to-type) still works. Transparency is native
  (ARGB buffer + `KEEBS_OSK_ALPHA`).
- **Stable app id**: SDL_SetAppMetadata pins the Wayland `app-id` to
  `keebs.osk`, so compositor rules can match the OSK reliably.
- **Positioning on niri** (compositor-side, not code): a window rule makes
  the OSK behave like a keyboard overlay:

  ```kdl
  window-rule {
      match app-id="keebs.osk"
      open-floating true
      open-focused false
      default-floating-position x=0 y=0 relative-to="bottom-center"
      border {
          off
      }
      shadow {
          off
      }
  }
  ```
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
