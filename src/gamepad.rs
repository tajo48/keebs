//! SDL gamepad rumble — the haptics transport that actually works on a
//! USB-tethered Steam Deck.
//!
//! The Python app's Deck haptics did NOT come from the 0x83/0x82 HID tone
//! reports (deck.py marks those "unverified whether the Deck honors them") —
//! they came from Sdl3GamepadSource._rumble_one: a USB-C-tethered Deck shows
//! up as a generic SDL gamepad, and SDL_RumbleGamepad buzzes it through the
//! kernel's force-feedback path. This module reproduces that.
//!
//! Rumble intensities/cadences mirror inputsrc.Sdl3GamepadSource exactly.

use std::os::raw::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sdl3::sdl;

struct Pad(*mut c_void);
unsafe impl Send for Pad {}

static PADS: Mutex<Vec<Pad>> = Mutex::new(Vec::new());
static INITED: AtomicBool = AtomicBool::new(false);

/// Init the gamepad subsystem and grab every connected pad. Hints mirror
/// adusk.main: background events so pads keep flowing without window focus,
/// and SDL's hidapi-steam driver OFF so it never fights us for Valve gear.
pub fn init() {
    let s = sdl();
    unsafe {
        for (name, value) in [
            ("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1"),
            ("SDL_JOYSTICK_HIDAPI_STEAM", "0"),
        ] {
            let n = std::ffi::CString::new(name).unwrap();
            let v = std::ffi::CString::new(value).unwrap();
            (s.set_hint)(n.as_ptr(), v.as_ptr());
        }
        if !(s.init_subsystem)(crate::sdl3::SDL_INIT_GAMEPAD) {
            eprintln!(
                "osk: SDL_InitSubSystem(GAMEPAD) failed: {}",
                s.get_error_string()
            );
            return;
        }
    }
    rescan();
    INITED.store(true, Ordering::SeqCst);
}

fn rescan() {
    let s = sdl();
    unsafe {
        let mut count: std::os::raw::c_int = 0;
        let ids = (s.get_gamepads)(&mut count);
        if ids.is_null() {
            return;
        }
        let mut pads = PADS.lock().unwrap();
        for i in 0..count {
            let id = *ids.add(i as usize);
            if (s.is_gamepad)(id) {
                let pad = (s.open_gamepad)(id);
                if !pad.is_null() {
                    pads.push(Pad(pad));
                }
            }
        }
        (s.sdl_free)(ids as *mut c_void);
        if !pads.is_empty() {
            println!("gamepad: {} pad(s) open for rumble", pads.len());
        }
    }
}

/// Light "key tap" tick.
pub fn haptic_tick() {
    rumble(0x0200, 0x0C00, 10);
}

/// Firmer "select" tick.
pub fn pad_click() {
    rumble(0x1A00, 0x2800, 24);
}

fn rumble(low: u16, high: u16, ms: u32) {
    if !INITED.load(Ordering::SeqCst) {
        return;
    }
    {
        let pads = PADS.lock().unwrap();
        if !pads.is_empty() {
            let s = sdl();
            unsafe {
                for pad in pads.iter() {
                    (s.rumble_gamepad)(pad.0, low, high, ms);
                }
            }
            return;
        }
    }
    // No pads open (first call, or hot-plug) — rescan once and try again.
    rescan();
    let pads = PADS.lock().unwrap();
    let s = sdl();
    unsafe {
        for pad in pads.iter() {
            (s.rumble_gamepad)(pad.0, low, high, ms);
        }
    }
}
