//! Key/mouse injection backend: direct `/dev/uinput` (the only path that
//! works under Wayland — XTest silently drops keys destined for native
//! Wayland apps).
//!
//! Rust port of `linux/steamcontroller/uinput.py`, minus the pynput fallback
//! (Linux-only crate). Requires `/dev/uinput` to be writable by the user
//! (most modern distros grant this via a uaccess ACL).

use std::io;
use std::os::raw::c_void;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::sc::{SciInput, buttons};

// uinput ioctl interface (values are architecture-stable on linux).
const UI_SET_EVBIT: u32 = 0x4004_5564;
const UI_SET_KEYBIT: u32 = 0x4004_5565;
const UI_SET_RELBIT: u32 = 0x4004_5566;
const UI_SET_ABSBIT: u32 = 0x4004_5567;
const UI_DEV_CREATE: u32 = 0x0000_5501;
const UI_DEV_DESTROY: u32 = 0x0000_5502;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;

const BUS_VIRTUAL: u16 = 0x06;

#[repr(C)]
#[derive(Clone, Copy)]
struct Timeval {
    sec: i64,
    usec: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time: Timeval,
    etype: u16,
    code: u16,
    value: i32,
}

#[repr(C)]
struct UinputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputUserDev {
    name: [u8; 80],
    id: UinputId,
    ff_effects_max: u32,
    absmax: [i32; 64],
    absmin: [i32; 64],
    absfuzz: [i32; 64],
    absflat: [i32; 64],
}

/// One opened virtual device (keyboard or mouse). Destroys the uinput device
/// on drop so events stop the moment the app exits.
struct RawUinput {
    fd: i32,
}

impl RawUinput {
    /// `keys`: EV_KEY codes to enable; `rels`: EV_REL codes; `abs`: (code,
    /// min, max) triplets for EV_ABS axes.
    fn create(
        name: &str,
        keys: &[u16],
        rels: &[u16],
        abs: &[(u16, i32, i32)],
        id: (u16, u16, u16),
    ) -> io::Result<Self> {
        let path = std::ffi::CString::new("/dev/uinput").unwrap();
        let mut fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if fd < 0 {
            fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY) };
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let dev = Self { fd };

        let set = |req: u32, arg: u32| -> io::Result<()> {
            let rc = unsafe { libc::ioctl(dev.fd, req as libc::c_ulong, arg as libc::c_int) };
            if rc < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        };

        set(UI_SET_EVBIT, EV_SYN as u32)?;
        set(UI_SET_EVBIT, EV_KEY as u32)?;
        for k in keys {
            set(UI_SET_KEYBIT, *k as u32)?;
        }
        if !rels.is_empty() {
            set(UI_SET_EVBIT, EV_REL as u32)?;
            for r in rels {
                set(UI_SET_RELBIT, *r as u32)?;
            }
        }
        if !abs.is_empty() {
            set(UI_SET_EVBIT, EV_ABS as u32)?;
            for (code, _, _) in abs {
                set(UI_SET_ABSBIT, *code as u32)?;
            }
        }
        let mut setup = UinputUserDev {
            name: [0; 80],
            id: UinputId {
                bustype: id.2,
                vendor: id.0,
                product: id.1,
                version: 1,
            },
            ff_effects_max: 0,
            absmax: [0; 64],
            absmin: [0; 64],
            absfuzz: [0; 64],
            absflat: [0; 64],
        };
        for (code, min, max) in abs {
            setup.absmin[*code as usize] = *min;
            setup.absmax[*code as usize] = *max;
        }
        let bytes = name.as_bytes();
        let len = bytes.len().min(79);
        setup.name[..len].copy_from_slice(&bytes[..len]);

        let rc = unsafe {
            libc::write(
                dev.fd,
                &setup as *const UinputUserDev as *const c_void,
                std::mem::size_of::<UinputUserDev>(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        set(UI_DEV_CREATE, 0)?;
        Ok(dev)
    }

    fn emit(&self, etype: u16, code: u16, value: i32) -> io::Result<()> {
        let ev = InputEvent {
            time: Timeval { sec: 0, usec: 0 },
            etype,
            code,
            value,
        };
        let rc = unsafe {
            libc::write(
                self.fd,
                &ev as *const InputEvent as *const c_void,
                std::mem::size_of::<InputEvent>(),
            )
        };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn syn(&self) -> io::Result<()> {
        self.emit(EV_SYN, 0, 0)
    }
}

impl Drop for RawUinput {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.fd, UI_DEV_DESTROY as libc::c_ulong);
            libc::close(self.fd);
        }
    }
}

// ---------------------------------------------------------------------------
// KEY_* name -> Linux keycode map (kernel input.h constants).
// ---------------------------------------------------------------------------

const KEYMAP: &[(&str, u16)] = &[
    ("KEY_ESC", 1),
    ("KEY_1", 2),
    ("KEY_2", 3),
    ("KEY_3", 4),
    ("KEY_4", 5),
    ("KEY_5", 6),
    ("KEY_6", 7),
    ("KEY_7", 8),
    ("KEY_8", 9),
    ("KEY_9", 10),
    ("KEY_0", 11),
    ("KEY_MINUS", 12),
    ("KEY_EQUAL", 13),
    ("KEY_BACKSPACE", 14),
    ("KEY_TAB", 15),
    ("KEY_Q", 16),
    ("KEY_W", 17),
    ("KEY_E", 18),
    ("KEY_R", 19),
    ("KEY_T", 20),
    ("KEY_Y", 21),
    ("KEY_U", 22),
    ("KEY_I", 23),
    ("KEY_O", 24),
    ("KEY_P", 25),
    ("KEY_LEFTBRACE", 26),
    ("KEY_RIGHTBRACE", 27),
    ("KEY_ENTER", 28),
    ("KEY_LEFTCTRL", 29),
    ("KEY_A", 30),
    ("KEY_S", 31),
    ("KEY_D", 32),
    ("KEY_F", 33),
    ("KEY_G", 34),
    ("KEY_H", 35),
    ("KEY_J", 36),
    ("KEY_K", 37),
    ("KEY_L", 38),
    ("KEY_SEMICOLON", 39),
    ("KEY_APOSTROPHE", 40),
    ("KEY_GRAVE", 41),
    ("KEY_LEFTSHIFT", 42),
    ("KEY_BACKSLASH", 43),
    ("KEY_Z", 44),
    ("KEY_X", 45),
    ("KEY_C", 46),
    ("KEY_V", 47),
    ("KEY_B", 48),
    ("KEY_N", 49),
    ("KEY_M", 50),
    ("KEY_COMMA", 51),
    ("KEY_DOT", 52),
    ("KEY_SLASH", 53),
    ("KEY_RIGHTSHIFT", 54),
    ("KEY_KPASTERISK", 55),
    ("KEY_LEFTALT", 56),
    ("KEY_SPACE", 57),
    ("KEY_CAPSLOCK", 58),
    ("KEY_F1", 59),
    ("KEY_F2", 60),
    ("KEY_F3", 61),
    ("KEY_F4", 62),
    ("KEY_F5", 63),
    ("KEY_F6", 64),
    ("KEY_F7", 65),
    ("KEY_F8", 66),
    ("KEY_F9", 67),
    ("KEY_F10", 68),
    ("KEY_NUMLOCK", 69),
    ("KEY_SCROLLLOCK", 70),
    ("KEY_KP7", 71),
    ("KEY_KP8", 72),
    ("KEY_KP9", 73),
    ("KEY_KPMINUS", 74),
    ("KEY_KP4", 75),
    ("KEY_KP5", 76),
    ("KEY_KP6", 77),
    ("KEY_KPPLUS", 78),
    ("KEY_KP1", 79),
    ("KEY_KP2", 80),
    ("KEY_KP3", 81),
    ("KEY_KP0", 82),
    ("KEY_KPDOT", 83),
    ("KEY_F11", 87),
    ("KEY_F12", 88),
    ("KEY_RIGHTCTRL", 97),
    ("KEY_KPSLASH", 98),
    ("KEY_SYSRQ", 99),
    ("KEY_RIGHTALT", 100),
    ("KEY_HOME", 102),
    ("KEY_UP", 103),
    ("KEY_PAGEUP", 104),
    ("KEY_LEFT", 105),
    ("KEY_RIGHT", 106),
    ("KEY_END", 107),
    ("KEY_DOWN", 108),
    ("KEY_PAGEDOWN", 109),
    ("KEY_INSERT", 110),
    ("KEY_DELETE", 111),
    ("KEY_MUTE", 113),
    ("KEY_VOLUMEDOWN", 114),
    ("KEY_VOLUMEUP", 115),
    ("KEY_POWER", 116),
    ("KEY_KPEQUAL", 117),
    ("KEY_LEFTMETA", 125),
    ("KEY_RIGHTMETA", 126),
    ("KEY_COMPOSE", 127),
    ("KEY_NEXTSONG", 163),
    ("KEY_PLAYPAUSE", 164),
    ("KEY_PREVIOUSSONG", 165),
    // adusk aliases that aren't 1:1 kernel names.
    ("KEY_LEFTWIN", 125), // KEY_LEFTMETA
    ("KEY_QUESTION", 53), // '?' is shift+'/'; same physical key
];

pub fn keycode(name: &str) -> Option<u16> {
    KEYMAP.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// Linux keycodes used by the OSK logic (same values as `KEYMAP`).
pub mod keys {
    pub const KEY_ESC: u16 = 1;
    pub const KEY_BACKSPACE: u16 = 14;
    pub const KEY_TAB: u16 = 15;
    pub const KEY_ENTER: u16 = 28;
    pub const KEY_LEFTCTRL: u16 = 29;
    pub const KEY_C: u16 = 46;
    pub const KEY_V: u16 = 47;
    pub const KEY_DOT: u16 = 52;
    pub const KEY_LEFTSHIFT: u16 = 42;
    pub const KEY_RIGHTSHIFT: u16 = 54;
    pub const KEY_LEFTALT: u16 = 56;
    pub const KEY_SPACE: u16 = 57;
    pub const KEY_CAPSLOCK: u16 = 58;
    pub const KEY_LEFTMETA: u16 = 125;
    pub const KEY_RIGHTALT: u16 = 100;
    pub const KEY_UP: u16 = 103;
    pub const KEY_LEFT: u16 = 105;
    pub const KEY_RIGHT: u16 = 106;
    pub const KEY_DOWN: u16 = 108;
    pub const KEY_PLAYPAUSE: u16 = 164;
    pub const KEY_VOLUMEUP: u16 = 115;
    pub const KEY_VOLUMEDOWN: u16 = 114;
    pub const KEY_PREVIOUSSONG: u16 = 165;
    pub const KEY_NEXTSONG: u16 = 163;
}

// ---------------------------------------------------------------------------
// Public API — mirrors the Python Keyboard/Mouse classes.
// ---------------------------------------------------------------------------

pub struct Keyboard {
    dev: Mutex<RawUinput>,
    caps_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Keyboard {
    pub fn new() -> io::Result<Self> {
        let codes: Vec<u16> = KEYMAP
            .iter()
            .map(|(_, c)| *c)
            .filter(|c| *c > 0 && *c < 0x2FF)
            .collect();
        let dev = RawUinput::create(
            "keebs-virtual-kb",
            &codes,
            &[],
            &[],
            (0x28DE, 0x0001, BUS_VIRTUAL),
        )?;
        // Compositors take a moment to notice a freshly created uinput
        // device; without a brief settle the first key event can be dropped.
        thread::sleep(Duration::from_millis(100));
        Ok(Self {
            dev: Mutex::new(dev),
            caps_hook: Mutex::new(None),
        })
    }

    /// Called whenever KEY_CAPSLOCK is injected: KWin/Wayland doesn't
    /// propagate the latched-caps state through XKB, so the OSK layer needs a
    /// manual signal to know when to flip its glyphs.
    pub fn set_caps_hook(&self, hook: Box<dyn Fn() + Send + Sync>) {
        *self.caps_hook.lock().unwrap() = Some(hook);
    }

    pub fn press_event(&self, keys: &[&str]) {
        self.emit_keys(keys, 1);
    }

    pub fn release_event(&self, keys: &[&str]) {
        self.emit_keys(keys, 0);
    }

    /// Press modifier+key as a chord, then release it (Ctrl+V, Win+. ...).
    pub fn tap_with_modifier(&self, modifier: &str, key: &str) {
        self.press_event(&[modifier]);
        self.press_event(&[key]);
        self.release_event(&[key]);
        self.release_event(&[modifier]);
    }

    fn emit_keys(&self, keys: &[&str], value: i32) {
        let mut caps = false;
        {
            let dev = self.dev.lock().unwrap();
            for name in keys {
                let Some(code) = keycode(name) else { continue };
                if let Err(e) = dev.emit(EV_KEY, code, value) {
                    eprintln!("uinput: {name} failed: {e}");
                }
                if *name == "KEY_CAPSLOCK" {
                    caps = true;
                }
            }
            let _ = dev.syn();
        }
        if caps {
            if let Some(hook) = self.caps_hook.lock().unwrap().as_ref() {
                hook();
            }
        }
    }

    /// Raw-keycode variants for the OSK manager (which works in u16 Linux
    /// keycodes instead of `KEY_*` names). Same caps_hook notification.
    pub fn press_code(&self, code: u16) {
        self.emit_code(code, 1);
    }

    pub fn release_code(&self, code: u16) {
        self.emit_code(code, 0);
    }

    pub fn tap_code(&self, code: u16) {
        self.press_code(code);
        self.release_code(code);
    }

    /// Lift every injectable key (idempotent). Runs on drop so the app can
    /// never leave a key logically held in the X server's global state —
    /// a stuck uinput Backspace makes the PHYSICAL Backspace dead until the
    /// app exits (observed in the wild).
    pub fn release_all(&self) {
        let codes: Vec<u16> = KEYMAP
            .iter()
            .map(|(_, c)| *c)
            .filter(|c| *c > 0 && *c < 0x2FF)
            .collect();
        let dev = self.dev.lock().unwrap();
        for c in codes {
            let _ = dev.emit(EV_KEY, c, 0);
        }
        let _ = dev.syn();
    }

    fn emit_code(&self, code: u16, value: i32) {
        let caps = code == crate::uinput::keys::KEY_CAPSLOCK && value == 1;
        {
            let dev = self.dev.lock().unwrap();
            if let Err(e) = dev.emit(EV_KEY, code, value) {
                eprintln!("uinput: key {code} failed: {e}");
            }
            let _ = dev.syn();
        }
        if caps {
            if let Some(hook) = self.caps_hook.lock().unwrap().as_ref() {
                hook();
            }
        }
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.release_all();
    }
}

pub struct Mouse {
    dev: Mutex<RawUinput>,
}

impl Mouse {
    pub fn new() -> io::Result<Self> {
        let dev = RawUinput::create(
            "keebs-virtual-mouse",
            &[BTN_LEFT, BTN_RIGHT, BTN_MIDDLE],
            &[REL_X, REL_Y, REL_WHEEL, REL_HWHEEL],
            &[],
            (0x28DE, 0x0002, BUS_VIRTUAL),
        )?;
        thread::sleep(Duration::from_millis(100));
        Ok(Self {
            dev: Mutex::new(dev),
        })
    }

    /// Relative cursor movement.
    pub fn move_rel(&self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        let dev = self.dev.lock().unwrap();
        if let Err(e) = dev.emit(EV_REL, REL_X, dx) {
            eprintln!("uinput: mouse move failed: {e}");
        }
        if let Err(e) = dev.emit(EV_REL, REL_Y, dy) {
            eprintln!("uinput: mouse move failed: {e}");
        }
        let _ = dev.syn();
    }

    /// Press/release a mouse button: `button` is "left", "right" or "middle".
    pub fn button(&self, button: &str, pressed: bool) {
        let code = match button {
            "left" => BTN_LEFT,
            "right" => BTN_RIGHT,
            "middle" => BTN_MIDDLE,
            _ => return,
        };
        let dev = self.dev.lock().unwrap();
        if let Err(e) = dev.emit(EV_KEY, code, if pressed { 1 } else { 0 }) {
            eprintln!("uinput: mouse {button} failed: {e}");
        }
        let _ = dev.syn();
    }

    pub fn press(&self, button: &str) {
        self.button(button, true);
    }

    pub fn release(&self, button: &str) {
        self.button(button, false);
    }

    /// Scroll wheel; +dy = up.
    pub fn scroll(&self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        let dev = self.dev.lock().unwrap();
        if dx != 0 {
            if let Err(e) = dev.emit(EV_REL, REL_HWHEEL, dx) {
                eprintln!("uinput: hscroll failed: {e}");
            }
        }
        if dy != 0 {
            if let Err(e) = dev.emit(EV_REL, REL_WHEEL, dy) {
                eprintln!("uinput: scroll failed: {e}");
            }
        }
        let _ = dev.syn();
    }
}

impl Drop for Mouse {
    fn drop(&mut self) {
        // Lift any held button — same stuck-state rationale as Keyboard.
        let dev = self.dev.lock().unwrap();
        for b in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
            let _ = dev.emit(EV_KEY, b, 0);
        }
        let _ = dev.syn();
    }
}

// ---------------------------------------------------------------------------
// Virtual gamepad (--pad mode): Xbox 360 layout so SDL / Steam Input / Proton
// auto-map it with zero configuration. Identity is Microsoft's X-Box 360 pad
// on purpose — a custom VID/PID would land as an "unknown controller".
// ---------------------------------------------------------------------------

const BUS_USB: u16 = 0x03;

const BTN_SOUTH: u16 = 0x130; // physical A
const BTN_EAST: u16 = 0x131; // physical B
const BTN_NORTH: u16 = 0x133; // physical X (xpad.c convention)
const BTN_WEST: u16 = 0x134; // physical Y
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c; // guide / STEAM
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
const BTN_DPAD_UP: u16 = 0x220;
const BTN_DPAD_DOWN: u16 = 0x221;
const BTN_DPAD_LEFT: u16 = 0x222;
const BTN_DPAD_RIGHT: u16 = 0x223;
// Back paddles land on the trigger-happy block — readable by everything,
// mapped by nothing by default.
const BTN_HAPPY1: u16 = 0x2c0;
const BTN_HAPPY2: u16 = 0x2c1;
const BTN_HAPPY3: u16 = 0x2c2;
const BTN_HAPPY4: u16 = 0x2c3;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02; // left trigger
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05; // right trigger

/// Fixed state-slot order: which code each slot carries, and whether it is
/// an EV_KEY button or an EV_ABS axis.
const PAD_SLOTS: &[(u16, bool)] = &[
    (BTN_SOUTH, false),
    (BTN_EAST, false),
    (BTN_NORTH, false),
    (BTN_WEST, false),
    (BTN_TL, false),
    (BTN_TR, false),
    (BTN_SELECT, false),
    (BTN_START, false),
    (BTN_MODE, false),
    (BTN_THUMBL, false),
    (BTN_THUMBR, false),
    (BTN_DPAD_UP, false),
    (BTN_DPAD_DOWN, false),
    (BTN_DPAD_LEFT, false),
    (BTN_DPAD_RIGHT, false),
    (BTN_HAPPY1, false),
    (BTN_HAPPY2, false),
    (BTN_HAPPY3, false),
    (BTN_HAPPY4, false),
    (ABS_X, true),
    (ABS_Y, true),
    (ABS_RX, true),
    (ABS_RY, true),
    (ABS_Z, true),
    (ABS_RZ, true),
];

pub struct Gamepad {
    dev: Mutex<RawUinput>,
    /// Last emitted value per slot — only deltas hit the wire (a 250 Hz
    /// Deck would otherwise spam ~25 events/frame into evdev consumers).
    prev: Mutex<Vec<i32>>,
}

impl Gamepad {
    pub fn new() -> io::Result<Self> {
        const STICK_MIN: i32 = i16::MIN as i32;
        const STICK_MAX: i32 = i16::MAX as i32;
        let keys: Vec<u16> = PAD_SLOTS
            .iter()
            .filter(|(_, a)| !a)
            .map(|(c, _)| *c)
            .collect();
        let abs: Vec<(u16, i32, i32)> = PAD_SLOTS
            .iter()
            .filter(|(_, a)| *a)
            .map(|(c, _)| {
                (
                    *c,
                    if *c == ABS_Z || *c == ABS_RZ {
                        0
                    } else {
                        STICK_MIN
                    },
                    STICK_MAX,
                )
            })
            .collect();
        let dev = RawUinput::create(
            "keebs-virtual-gamepad",
            &keys,
            &[],
            &abs,
            (0x045e, 0x028e, BUS_USB),
        )?;
        // Same settle rationale as Keyboard/Mouse: give evdev consumers a
        // beat to enumerate before the first real frame.
        thread::sleep(Duration::from_millis(100));
        let n = PAD_SLOTS.len();
        Ok(Self {
            dev: Mutex::new(dev),
            prev: Mutex::new(vec![0; n]),
        })
    }

    /// Translate one controller frame into the virtual pad.
    /// `pads_as_sticks`: classic 2015 has no sticks — its pads ARE the
    /// sticks; Deck/Triton use their real sticks and keep the pads out of
    /// the axis path (rest offsets would drift the view in-game).
    pub fn emit_sci(&self, s: &SciInput, pads_as_sticks: bool) {
        let b = s.buttons;
        let bit = |m: u32| i32::from(b & m != 0);
        // Pad CLICK feeds the stick buttons in pads-as-sticks mode (pressing
        // the pad = L3/R3 press, like lizard's own behavior).
        let thumb_l = bit(buttons::L3)
            | if pads_as_sticks {
                bit(buttons::LPAD)
            } else {
                0
            };
        let thumb_r = bit(buttons::R3)
            | if pads_as_sticks {
                bit(buttons::RPAD)
            } else {
                0
            };
        let (lx, ly, rx, ry) = if pads_as_sticks {
            (
                s.lpad_x as i32,
                s.lpad_y as i32,
                s.rpad_x as i32,
                s.rpad_y as i32,
            )
        } else {
            (
                s.lstick_x as i32,
                s.lstick_y as i32,
                s.rstick_x as i32,
                s.rstick_y as i32,
            )
        };
        let state = [
            bit(buttons::A),
            bit(buttons::B),
            bit(buttons::X),
            bit(buttons::Y),
            bit(buttons::LB),
            bit(buttons::RB),
            bit(buttons::VIEW),
            bit(buttons::START),
            bit(buttons::STEAM),
            thumb_l,
            thumb_r,
            bit(buttons::DPAD_UP),
            bit(buttons::DPAD_DOWN),
            bit(buttons::DPAD_LEFT),
            bit(buttons::DPAD_RIGHT),
            bit(buttons::LGRIP1),
            bit(buttons::LGRIP2),
            bit(buttons::RGRIP1),
            bit(buttons::RGRIP2),
            lx,
            ly,
            rx,
            ry,
            s.ltrig as i32,
            s.rtrig as i32,
        ];
        debug_assert_eq!(state.len(), PAD_SLOTS.len());
        let mut prev = self.prev.lock().unwrap();
        let dev = self.dev.lock().unwrap();
        let mut changed = false;
        for (i, val) in state.iter().enumerate() {
            if prev[i] == *val {
                continue;
            }
            let (code, is_abs) = PAD_SLOTS[i];
            let rc = if is_abs {
                dev.emit(EV_ABS, code, *val)
            } else {
                dev.emit(EV_KEY, code, *val)
            };
            if let Err(e) = rc {
                eprintln!("uinput: gamepad slot {i} failed: {e}");
            }
            prev[i] = *val;
            changed = true;
        }
        if changed {
            let _ = dev.syn();
        }
    }

    /// Center everything (idempotent). Runs on drop so the app can never
    /// leave a button stuck or a stick floored in a running game.
    pub fn release_all(&self) {
        let mut prev = self.prev.lock().unwrap();
        let dev = self.dev.lock().unwrap();
        for (i, (code, is_abs)) in PAD_SLOTS.iter().enumerate() {
            if prev[i] == 0 {
                continue;
            }
            let rc = if *is_abs {
                // Sticks/triggers rest at 0 per our declared ranges… except
                // stick axes whose neutral is 0 too (min is negative) — 0 is
                // correct for all six axes.
                dev.emit(EV_ABS, *code, 0)
            } else {
                dev.emit(EV_KEY, *code, 0)
            };
            if let Err(e) = rc {
                eprintln!("uinput: gamepad release slot {i} failed: {e}");
            }
            prev[i] = 0;
        }
        let _ = dev.syn();
    }
}

impl Drop for Gamepad {
    fn drop(&mut self) {
        self.release_all();
    }
}
