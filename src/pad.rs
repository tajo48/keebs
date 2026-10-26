//! `--pad` mode: every supported controller (Deck / Triton 2026 / classic
//! 2015) forwards into ONE virtual Xbox 360 gamepad, so games and Steam see
//! a standard controller instead of the raw hardware.
//!
//! Why this exists: keebs takes the controller's vendor interface exclusive
//! (libusb claim detaches usbhid) and suppresses the firmware's lizard
//! mouse/keyboard via kernel EVIOCGRAB — both hostile to a running Steam
//! client, which wants to own the same device. In pad mode that hostility
//! becomes the FEATURE: the real hardware disappears from the desktop
//! entirely (grabbed evdev nodes, claimed vendor interface) and the only
//! thing anyone sees is "Microsoft X-Box 360 pad" from uinput, fed by keebs.
//! Steam Input configures the virtual pad like any other controller; the
//! real Deck/Triton/classic never reach it.
//!
//! Button mapping notes:
//! - Face buttons follow the kernel xpad.c convention (physical X -> BTN_X
//!   code 0x133, physical Y -> BTN_Y code 0x134) — that is what SDL's
//!   default Xbox mappings expect.
//! - Back paddles land on BTN_TRIGGER_HAPPY1..4: visible to evdev consumers,
//!   unmapped by default, so they can't fire accidental in-game actions.
//! - The STEAM button is BTN_MODE (guide). QAM has no gamepad equivalent —
//!   dropped.
//! - Classic 2015 has no sticks: its pads ARE the sticks (pad click = L3/R3).
//!   Deck/Triton use their real sticks; pads stay out of the axis path.

use std::sync::Arc;
use std::time::Duration;

use crate::classic::{self, ClassicController};
use crate::deck::{self, DeckController};
use crate::sc::SciInput;
use crate::stop::StopFlag;
use crate::triton::{self, SteamController};
use crate::uinput::Gamepad;

fn run_deck(stop: Arc<StopFlag>, pad: Arc<Gamepad>) {
    loop {
        if !deck::deck_present() {
            if stop.wait(Duration::from_secs(2)) {
                break;
            }
            continue;
        }
        crate::diag::log("pad: Steam Deck (USB tethered, 28de:1205) attached");
        // passive=false + set_lizard(false): grab the Deck's evdev nodes so
        // its firmware mouse/keys never reach the desktop.
        let Ok(ctrl) = DeckController::new(false, None, Arc::clone(&stop)) else {
            if stop.wait(Duration::from_secs(2)) {
                break;
            }
            continue;
        };
        let ctrl = Arc::new(ctrl);
        ctrl.set_lizard(false);
        ctrl.run(&mut |_c, sci: &SciInput| pad.emit_sci(sci, false));
        if stop.wait(Duration::from_secs(2)) {
            break;
        }
    }
}

fn run_triton(stop: Arc<StopFlag>, pad: Arc<Gamepad>) {
    loop {
        if !triton::triton_present() {
            if stop.wait(Duration::from_secs(2)) {
                break;
            }
            continue;
        }
        crate::diag::log("pad: Steam Controller 2026 (dongle/wired) attached");
        let Ok(ctrl) = SteamController::new(false, Arc::clone(&stop)) else {
            if stop.wait(Duration::from_secs(2)) {
                break;
            }
            continue;
        };
        let ctrl = Arc::new(ctrl);
        ctrl.set_lizard(false);
        ctrl.run(&mut |_c, sci: &SciInput| pad.emit_sci(sci, false));
        if stop.wait(Duration::from_secs(2)) {
            break;
        }
    }
}

fn run_classic(stop: Arc<StopFlag>, pad: Arc<Gamepad>) {
    loop {
        if !classic::classic_present() {
            if stop.wait(Duration::from_secs(2)) {
                break;
            }
            continue;
        }
        crate::diag::log("pad: Steam Controller 2015 (dongle/wired) attached");
        let Ok(ctrl) = ClassicController::new(false, Arc::clone(&stop)) else {
            if stop.wait(Duration::from_secs(2)) {
                break;
            }
            continue;
        };
        let ctrl = Arc::new(ctrl);
        ctrl.set_lizard(false);
        ctrl.run(&mut |_c, sci: &SciInput| pad.emit_sci(sci, true));
        if stop.wait(Duration::from_secs(2)) {
            break;
        }
    }
}

pub fn run(stop: Arc<StopFlag>) {
    let pad = match Gamepad::new() {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("pad: cannot create virtual gamepad ({e}); is /dev/uinput writable?");
            return;
        }
    };
    println!("pad: virtual Xbox 360 gamepad live — real controllers hidden");

    {
        let stop = Arc::clone(&stop);
        let pad = Arc::clone(&pad);
        std::thread::spawn(move || run_deck(stop, pad));
    }
    {
        let stop = Arc::clone(&stop);
        let pad = Arc::clone(&pad);
        std::thread::spawn(move || run_triton(stop, pad));
    }
    {
        let stop = Arc::clone(&stop);
        let pad = Arc::clone(&pad);
        std::thread::spawn(move || run_classic(stop, pad));
    }

    while !stop.is_set() {
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("bye");
}
