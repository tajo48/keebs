# keebs

Rust port of the SteamlessKeyboard **Linux input core** (phase 1 of the
rewrite of `linux/` in Rust).

## Modules

| module | ports | what it does |
|--------|-------|--------------|
| `sc` | `steamcontroller/__init__.py` (protocol) + `deck.py` (parsing) | SCButtons bit flags, `SciInput`, Triton/Deck/battery parsers, lizard / turn-off / haptic / rumble report builders |
| `triton` | `steamcontroller/__init__.py` (`SteamController`) | Driver for the Triton dongle (0x1304) + wired 2026 controller (0x1302): interface probing, read loop, battery freshness, haptics, chime, self-expiring rumble + keepalive |
| `deck` | `steamcontroller/deck.py` (`DeckController`) | USB-tethered Steam Deck (0x1205): streaming-interface probe, stick auto-centering, lizard refresh watchdog, best-effort haptics |
| `events` | `steamcontroller/events.py` | Button-edge → key mapping |
| `uinput` | `steamcontroller/uinput.py` | Direct `/dev/uinput` key/mouse injection (Wayland-safe); same `KEY_*` name map incl. the `KEY_LEFTWIN`/`KEY_QUESTION` aliases |
| `stop` | `threading.Event` usage | Co-operative stop flag shared between drivers and the app |

## Not ported yet (remaining phases)

- **On-screen keyboard GUI** — `adusk/screen.py` (SDL renderer, skins,
  glyphs, open animation), `vkb.py` (key layout + cursor stepping),
  `skins.py`, and the X11 no-focus/click-through window tricks in
  `adusk.py`
- **Controller logic / chords** — `adusk/controller.py` (trackpad cursor,
  Steam chords, gamepad↔PC switching), `inputsrc.py`
- **Tray icon** — `tray.py` / `tray_linux.py`
- **Gamepad mode** — virtual Xbox pad (Windows uses ViGEmBus; a Linux
  equivalent would use a uinput Xbox 360 pad or the HID gamepad approach)
- **Windows tree** — `windows/` (ViGEm client, HidHide, lock-screen OSK)

## Building / running

```sh
cd keebs; cargo run --release -- --inject
```

`--inject` creates the virtual keyboard/mouse and maps **LGRIP1 (L4
paddle) → Left Shift** (parity with adusk's default). Without it, the
binary just prints button edges and battery status.

Requires write access to `/dev/uinput` for injection (uaccess ACL on most
distros) and read access to the controller's hidraw nodes.

### Dependencies

```sh
cargo add hidapi@2.6 --no-default-features --features linux-static-hidraw
cargo add ctrlc@3 libc@0.2
```

The hidraw hidapi backend is deliberate: the libusb backend duplicates the
report-ID byte in `SET_REPORT` payloads, which Triton firmware rejects
(the Python driver hit the same wall and switched to hidraw).

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
