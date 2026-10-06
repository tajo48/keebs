//! keebs — Rust port of the SteamlessKeyboard Linux input core.
//!
//! Phase 1 binary: a passive controller listener (the chord-watcher role).
//! Detects whichever Valve device is present, streams its input, prints
//! button edges and battery status, and — with `--inject` — maps buttons to
//! keys via uinput (works under Wayland).
//!
//! Run: cargo run -- [--inject]   (Ctrl-C for a clean lizard restore)

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

fn main() {
    let inject = std::env::args().any(|a| a == "--inject");
    let stop = Arc::new(StopFlag::new());
    {
        let stop = Arc::clone(&stop);
        if let Err(e) = ctrlc::set_handler(move || stop.signal()) {
            eprintln!("warning: no Ctrl-C handler ({e})");
        }
    }

    // passive=true = desktop/chord-watcher posture: firmware lizard stays ON,
    // the trackpads keep driving the system mouse. The OSK layer will pass
    // false to own the controller.
    let passive = true;

    let mut mapper = if inject {
        match EventMapper::new() {
            Ok(mut m) => {
                // Parity with adusk: LGRIP1 (L4 paddle) -> Left Shift.
                m.set_button_action(buttons::LGRIP1, "KEY_LEFTSHIFT");
                Some(m)
            }
            Err(e) => {
                eprintln!("uinput init failed ({e}); running print-only");
                eprintln!("hint: check permissions on /dev/uinput");
                None
            }
        }
    } else {
        None
    };

    println!("keebs — SteamlessKeyboard input core (Rust)");
    println!(
        "injection: {}",
        if mapper.is_some() {
            "on (LGRIP1/L4 -> LeftShift)"
        } else {
            "off (pass --inject)"
        }
    );

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
