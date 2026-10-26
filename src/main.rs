//! keebs — Rust port of the SteamlessKeyboard Linux app.
//!
//! Default: the on-screen keyboard (phase 2) — SDL3 window + controller
//! chords (Steam+X opens, B/LGRIP closes, Steam-alone closes, L2=Shift,
//! R2=Enter, pads/sticks navigate, right stick = mouse).
//!
//! `--listener`: phase-1 passive device listener (prints button edges and
//! battery, LGRIP1/L4 -> Left Shift via uinput).
//! `--probe-sc`: guided calibration for the classic (2015) Steam Controller.
//! `--pad`: virtual Xbox 360 gamepad — real controllers hidden from the
//! desktop, everything forwarded into one standard pad (Steam-safe mode).

use std::sync::Arc;
use std::time::Duration;

use keebs::classic::{self, ClassicController};
use keebs::deck::{self, DeckController};
use keebs::events::EventMapper;
use keebs::sc::{self, SciInput, buttons};
use keebs::stop::StopFlag;
use keebs::triton::{self, SteamController};

fn report_edges(dev: &str, prev: &mut u32, sci: &SciInput) {
    let xor = *prev ^ sci.buttons;
    let pressed = xor & sci.buttons;
    let released = xor & *prev;
    if pressed != 0 {
        println!("  [{dev}] +{}", sc::button_names(pressed).join("+"));
    }
    if released != 0 {
        println!("  [{dev}] -{}", sc::button_names(released).join("+"));
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

/// Per-device uinput mapper (LGRIP1/L4 -> Left Shift, adusk parity).
/// One per listener thread: EventMapper owns a Keyboard and is not Sync.
fn make_mapper() -> Option<EventMapper> {
    match EventMapper::new() {
        Ok(mut m) => {
            m.set_button_action(buttons::LGRIP1, "KEY_LEFTSHIFT");
            Some(m)
        }
        Err(e) => {
            eprintln!("uinput init failed ({e}); running print-only");
            None
        }
    }
}

/// One listener thread per device kind — ALL attached devices print edges
/// simultaneously (the old version was Deck-first and hid the Steam
/// Controller whenever a Deck was tethered). passive=true: lizard stays ON.
fn listener(stop: Arc<StopFlag>) {
    println!("injection: on (LGRIP1/L4 -> LeftShift, per device)");

    // Deck thread.
    {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            loop {
                if !deck::deck_present() {
                    if stop.wait(Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                }
                keebs::diag::log("device: Steam Deck (USB tethered, 28de:1205)");
                let Ok(ctrl) = DeckController::new(true, None, Arc::clone(&stop)) else {
                    if stop.wait(Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                };
                let ctrl = Arc::new(ctrl);
                let mut prev = 0u32;
                let mut mapper = make_mapper();
                ctrl.run(&mut |_c, sci| {
                    report_edges("deck", &mut prev, sci);
                    if let Some(m) = mapper.as_mut() {
                        m.process(sci);
                    }
                });
                if stop.wait(Duration::from_secs(2)) {
                    break;
                }
            }
        });
    }

    // Triton (SC 2026) thread, with battery printer.
    {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            loop {
                if !triton::triton_present() {
                    if stop.wait(Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                }
                keebs::diag::log("device: Steam Controller 2026 (dongle/wired)");
                let Ok(ctrl) = SteamController::new(true, Arc::clone(&stop)) else {
                    if stop.wait(Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                };
                let ctrl = Arc::new(ctrl);
                spawn_battery_printer(&ctrl);
                let mut prev = 0u32;
                let mut mapper = make_mapper();
                ctrl.run(&mut |_c, sci| {
                    report_edges("sc2026", &mut prev, sci);
                    if let Some(m) = mapper.as_mut() {
                        m.process(sci);
                    }
                });
                if stop.wait(Duration::from_secs(2)) {
                    break;
                }
            }
        });
    }

    // Classic (SC 2015) thread.
    {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            loop {
                if !classic::classic_present() {
                    if stop.wait(Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                }
                keebs::diag::log(
                    "device: Steam Controller 2015 (dongle 28de:1102 / wired 28de:1142)",
                );
                let Ok(ctrl) = ClassicController::new(true, Arc::clone(&stop)) else {
                    if stop.wait(Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                };
                let ctrl = Arc::new(ctrl);
                // No battery printer: the classic battery report id is uncalibrated.
                let mut prev = 0u32;
                let mut mapper = make_mapper();
                ctrl.run(&mut |_c, sci| {
                    report_edges("sc2015", &mut prev, sci);
                    if let Some(m) = mapper.as_mut() {
                        m.process(sci);
                    }
                });
                if stop.wait(Duration::from_secs(2)) {
                    break;
                }
            }
        });
    }

    while !stop.is_set() {
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("bye");
}

fn main() {
    // GNOME autostart hands the app startup-notification / xdg-activation
    // tokens (DESKTOP_STARTUP_ID / XDG_ACTIVATION_TOKEN, plus
    // DESKTOP_AUTOSTART_ID — the one gnome-session actually sets for
    // /etc/xdg/autostart entries per the autostart spec). If the window is
    // mapped with a redeemable token, the compositor focuses the OSK and
    // every injected key lands in ourselves — the "window is always in
    // focus, must restart keebs after login" bug. A terminal launch passes
    // no token, which is why the manually restarted instance behaves. Strip
    // them before anything SDL sees the environment. (The .desktop Exec in
    // Flake/modules/programs/keebs.nix strips the same trio for older
    // binaries that lack this block.)
    // SAFETY: env mutation is UB if other threads are reading the
    // environment — this runs at the very top of main(), before any thread
    // spawn, so nothing else exists yet.
    unsafe {
        std::env::remove_var("DESKTOP_STARTUP_ID");
        std::env::remove_var("XDG_ACTIVATION_TOKEN");
        std::env::remove_var("DESKTOP_AUTOSTART_ID");
    }

    keebs::diag::start();
    let listener_mode = std::env::args().any(|a| a == "--listener");
    let probe_mode = std::env::args().any(|a| a == "--probe-sc");
    let pad_mode = std::env::args().any(|a| a == "--pad");
    let stop = Arc::new(StopFlag::new());
    {
        let stop = Arc::clone(&stop);
        if let Err(e) = ctrlc::set_handler(move || stop.signal()) {
            eprintln!("warning: no Ctrl-C handler ({e})");
        }
    }

    if probe_mode {
        classic::probe();
        return;
    }

    if pad_mode {
        println!("keebs — virtual gamepad mode");
        keebs::pad::run(stop);
        return;
    }

    if listener_mode {
        println!("keebs — listener mode (phase 1)");
        listener(stop);
    } else {
        println!("keebs — on-screen keyboard (Rust port)");
        keebs::osk_main::run(stop);
    }
}
