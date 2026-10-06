//! keebs — Rust port of the SteamlessKeyboard Linux app.
//!
//! Default: the on-screen keyboard (phase 2) — SDL3 window + controller
//! chords (Steam+X opens, B/LGRIP closes, Steam-alone closes, L2=Shift,
//! R2=Enter, pads/sticks navigate, right stick = mouse).
//!
//! `--listener`: phase-1 passive device listener (prints button edges and
//! battery, LGRIP1/L4 -> Left Shift via uinput).

use std::sync::Arc;
use std::time::Duration;

use keebs::deck::{self, DeckController};
use keebs::events::EventMapper;
use keebs::sc::{self, SciInput, buttons};
use keebs::stop::StopFlag;
use keebs::triton::{self, SteamController};

fn report_edges(prev: &mut u32, sci: &SciInput) {
    let xor = *prev ^ sci.buttons;
    let pressed = xor & sci.buttons;
    let released = xor & *prev;
    if pressed != 0 {
        println!("  +{}", sc::button_names(pressed).join("+"));
    }
    if released != 0 {
        println!("  -{}", sc::button_names(released).join("+"));
    }
    *prev = sci.buttons;
}

fn spawn_battery_printer(ctrl: &Arc<SteamController>) {
    // Held via Weak so the thread dies with its controller instance when the
    // reconnect loop swaps devices.
    let weak = Arc::downgrade(ctrl);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let Some(ctrl) = weak.upgrade() else { break };
            if ctrl.stop.is_set() {
                break;
            }
            if let Some(b) = ctrl.get_battery() {
                println!(
                    "battery: {}%{} ({} mV)",
                    b.percent,
                    if b.charging { " charging" } else { "" },
                    b.voltage_mv
                );
            }
        }
    });
}

fn listener(stop: Arc<StopFlag>) {
    let mut mapper = match EventMapper::new() {
        Ok(mut m) => {
            // Parity with adusk: LGRIP1 (L4 paddle) -> Left Shift.
            m.set_button_action(buttons::LGRIP1, "KEY_LEFTSHIFT");
            Some(m)
        }
        Err(e) => {
            eprintln!("uinput init failed ({e}); running print-only");
            None
        }
    };
    println!(
        "injection: {}",
        if mapper.is_some() {
            "on (LGRIP1/L4 -> LeftShift)"
        } else {
            "off"
        }
    );

    // passive=true = desktop/chord-watcher posture: firmware lizard stays ON.
    let passive = true;

    while !stop.is_set() {
        if deck::deck_present() {
            println!("device: Steam Deck (USB tethered, 28de:1205)");
            let ctrl = Arc::new(
                DeckController::new(passive, None, Arc::clone(&stop)).expect("hidapi init failed"),
            );
            let mut prev = 0u32;
            ctrl.run(&mut |_c, sci| {
                report_edges(&mut prev, sci);
                if let Some(m) = mapper.as_mut() {
                    m.process(sci);
                }
            });
        } else if triton::triton_present() {
            println!("device: Steam Controller 2026 (dongle/wired)");
            let ctrl = Arc::new(
                SteamController::new(passive, Arc::clone(&stop)).expect("hidapi init failed"),
            );
            spawn_battery_printer(&ctrl);
            let mut prev = 0u32;
            ctrl.run(&mut |_c, sci| {
                report_edges(&mut prev, sci);
                if let Some(m) = mapper.as_mut() {
                    m.process(sci);
                }
            });
        } else {
            println!("no controller present; retrying every 2 s…");
        }
        if stop.wait(Duration::from_secs(2)) {
            break;
        }
    }
    println!("bye");
}

fn main() {
    let listener_mode = std::env::args().any(|a| a == "--listener");
    let stop = Arc::new(StopFlag::new());
    {
        let stop = Arc::clone(&stop);
        if let Err(e) = ctrlc::set_handler(move || stop.signal()) {
            eprintln!("warning: no Ctrl-C handler ({e})");
        }
    }

    if listener_mode {
        println!("keebs — listener mode (phase 1)");
        listener(stop);
    } else {
        println!("keebs — on-screen keyboard (Rust port)");
        keebs::osk_main::run(stop);
    }
}
