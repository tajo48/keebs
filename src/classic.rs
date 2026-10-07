//! Original Steam Controller (2015) input driver — wireless dongle
//! (PID 0x1102) and wired (PID 0x1142).
//!
//! Third sibling of [`crate::triton`] / [`crate::deck`]: reads the classic
//! 64-byte 0x01 frames and normalizes them into [`crate::sc::SciInput`] so
//! the OSK manager is device-agnostic. The wire tables in
//! [`crate::sc::parse_classic`] are PROVISIONAL — run `keebs --probe-sc`
//! against live hardware (guided hold-each-input calibration, the same
//! methodology the Deck needed: calibrate_deck 1-6) and fix the offsets if
//! the decode disagrees.
//!
//! Classic hardware notes:
//! - No sticks, no D-pad, no L3/QAM — the pads are the pointer/cursor, which
//!   is exactly how the original Steam OSK drove this controller.
//! - Runtime lizard writes are the documented norm on this hardware (the
//!   Steam client itself wrote them constantly); the persistence trap
//!   observed on Triton does not apply.
//! - Haptics: the classic 0x8F "singer" report shape is NOT implemented —
//!   the haptic entry points are graceful no-ops until calibrated.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use crate::sc::{
    CLASSIC_DISABLE_LIZARD_REPORT, CLASSIC_ENABLE_LIZARD_REPORT, PRODUCT_ID_DONGLE,
    PRODUCT_ID_WIRED_2015, VENDOR_ID, parse_classic,
};
use crate::stop::StopFlag;

static ABSENCE_NOTED: AtomicBool = AtomicBool::new(false);

/// True while any classic Steam Controller is enumerable (cheap HID list).
pub fn classic_present() -> bool {
    match HidApi::new() {
        Ok(api) => api.device_list().any(|d| {
            d.vendor_id() == VENDOR_ID
                && (d.product_id() == PRODUCT_ID_DONGLE || d.product_id() == PRODUCT_ID_WIRED_2015)
        }),
        Err(_) => false,
    }
}

/// Vendor-specific HID data interfaces (usage page 0xFF00 usage 1, or a
/// hidraw-style 0/0) — same shape the Triton driver probes.
fn candidates(api: &HidApi) -> Vec<hidapi::DeviceInfo> {
    let mut out: Vec<hidapi::DeviceInfo> = api
        .device_list()
        .filter(|info| {
            info.vendor_id() == VENDOR_ID
                && (info.product_id() == PRODUCT_ID_DONGLE
                    || info.product_id() == PRODUCT_ID_WIRED_2015)
        })
        .filter(|info| {
            let (up, us) = (info.usage_page(), info.usage());
            (up == 0xFF00 && us == 1) || (up == 0 && us == 0)
        })
        .cloned()
        .collect();
    out.sort_by_key(|c| c.interface_number());
    out
}

pub struct ClassicController {
    pub stop: Arc<StopFlag>,
    api: HidApi,
    inner: Mutex<Option<HidDevice>>,
    opened: AtomicBool,
    passive: bool,
    lizard_enabled: AtomicBool,
}

impl ClassicController {
    /// `passive=true` keeps firmware kb/mouse (lizard) ON — the desktop-mode /
    /// chord-watcher posture. `false` (OSK mode) disables lizard at open.
    pub fn new(passive: bool, stop: Arc<StopFlag>) -> Result<Self, hidapi::HidError> {
        Ok(Self {
            api: HidApi::new()?,
            stop,
            inner: Mutex::new(None),
            opened: AtomicBool::new(false),
            passive,
            lizard_enabled: AtomicBool::new(passive),
        })
    }

    fn with_dev<R>(&self, f: impl FnOnce(&HidDevice) -> R) -> Option<R> {
        let guard = self.inner.lock().unwrap();
        guard.as_ref().map(f)
    }

    fn send_lizard(&self, enabled: bool) {
        let report = if enabled {
            &CLASSIC_ENABLE_LIZARD_REPORT
        } else {
            &CLASSIC_DISABLE_LIZARD_REPORT
        };
        let _ = self.with_dev(|dev| dev.send_feature_report(report));
    }

    /// Toggle lizard (firmware mouse/kb). The manager re-asserts on change or
    /// every ~2 s; classic firmware has no known persistence trap.
    pub fn set_lizard(&self, enabled: bool) {
        self.lizard_enabled.store(enabled, Ordering::SeqCst);
        self.send_lizard(enabled);
    }

    /// `addExit()` equivalent: break the read loop and tear down.
    pub fn add_exit(&self) {
        self.stop.signal();
    }

    // -- haptics (not yet implemented: 0x8F singer report uncalibrated) ----

    fn haptic_unavailable() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            println!(
                "classic: haptics not implemented yet (0x8F singer report \
                 uncalibrated) — ignoring haptic requests"
            );
        });
    }

    pub fn haptic_click(&self, _freq_hz: u32, _gain: i8, _count: u16) {
        Self::haptic_unavailable();
    }

    pub fn haptic_pad_click(&self) {
        Self::haptic_unavailable();
    }

    /// Classic battery rides a different report id than Triton's 0x43 —
    /// pending calibration; the listener printer handles `None` fine.
    pub fn get_battery(&self) -> Option<crate::sc::Battery> {
        None
    }

    pub fn is_live(&self) -> bool {
        self.opened.load(Ordering::SeqCst) && self.inner.lock().unwrap().is_some()
    }

    // -- main loop ----------------------------------------------------------

    /// Blocks while the controller streams input, invoking `cb` per frame.
    /// Returns when the device disappears or [`Self::add_exit`] fires.
    pub fn run(self: &Arc<Self>, cb: &mut dyn FnMut(&Arc<Self>, &crate::sc::SciInput)) {
        // Presence gate (same rationale as the Triton driver): never attempt
        // the open when no classic controller is even enumerable.
        let pids = crate::sc::present_product_ids(&self.api);
        if !(pids.contains(&PRODUCT_ID_DONGLE) || pids.contains(&PRODUCT_ID_WIRED_2015)) {
            if !ABSENCE_NOTED.swap(true, Ordering::SeqCst) {
                println!(
                    "classic: no Steam Controller 2015 present \
                     (looking for PID 0x{PRODUCT_ID_DONGLE:04X} dongle / \
                     0x{PRODUCT_ID_WIRED_2015:04X} wired)"
                );
            }
            return;
        }
        ABSENCE_NOTED.store(false, Ordering::SeqCst);

        // Open the first responsive data interface: the dongle exposes silent
        // slots (same behavior the Triton driver works around).
        let mut dev: Option<HidDevice> = None;
        for c in candidates(&self.api) {
            if let Ok(d) = self.api.open_path(c.path()) {
                // Probe: a live data interface streams 0x01 frames promptly.
                let mut buf = [0u8; 64];
                match d.read_timeout(&mut buf, 300) {
                    Ok(n) if n > 0 && buf[0] == crate::sc::CLASSIC_INPUT_STATUS => {
                        dev = Some(d);
                        break;
                    }
                    _ => continue,
                }
            }
        }
        let Some(dev) = dev else {
            println!("classic: no responsive data interface found");
            return;
        };
        *self.inner.lock().unwrap() = Some(dev);
        self.opened.store(true, Ordering::SeqCst);

        if !self.passive {
            self.send_lizard(false);
        }

        loop {
            if self.stop.is_set() {
                break;
            }
            let mut buf = [0u8; 64];
            // Lock held across the read syscall (<= 200 ms) — same policy as
            // the Triton driver.
            let n = {
                let guard = self.inner.lock().unwrap();
                let Some(dev) = guard.as_ref() else { break };
                match dev.read_timeout(&mut buf, 200) {
                    Ok(n) => n,
                    Err(e) => {
                        println!("classic: read error: {e}");
                        break;
                    }
                }
            };
            if n == 0 {
                continue;
            }
            if let Some(sci) = parse_classic(&buf[..n]) {
                cb(self, &sci);
            }
        }

        // Teardown: restore lizard (no haptic stops — haptics are no-ops).
        self.opened.store(false, Ordering::SeqCst);
        let dev = self.inner.lock().unwrap().take();
        if let Some(dev) = dev
            && !self.passive
        {
            let _ = dev.send_feature_report(&CLASSIC_ENABLE_LIZARD_REPORT);
        }
    }
}

// ---------------------------------------------------------------------------
// Guided calibration probe (`keebs --probe-sc`).
//
// Ports the calibrate_deck*.py methodology: capture a hands-off baseline,
// then hold each input through a timed window and report (a) which BIT
// positions flipped and (b) which u16 offsets swung — everything needed to
// confirm or fix the PROVISIONAL tables in sc.rs.
// ---------------------------------------------------------------------------

const PROBE_PHASES: &[(&str, &str)] = &[
    ("REST", "hands completely OFF the controller"),
    ("A", "hold A"),
    ("B", "hold B"),
    ("X", "hold X"),
    ("Y", "hold Y"),
    ("LB", "hold LEFT bumper"),
    ("RB", "hold RIGHT bumper"),
    ("LT", "hold LEFT trigger FULLY pulled"),
    ("RT", "hold RIGHT trigger FULLY pulled"),
    ("BACK", "hold the SELECT/BACK button (right of STEAM)"),
    ("STEAM", "hold the STEAM button"),
    ("START", "hold START (left of STEAM)"),
    ("LGRIP", "hold the LEFT back paddle"),
    ("RGRIP", "hold the RIGHT back paddle"),
    ("LPAD-TOUCH", "rest a thumb ON the LEFT trackpad, still"),
    ("LPAD-CLICK", "physically CLICK the LEFT trackpad and hold"),
    ("RPAD-TOUCH", "rest a thumb ON the RIGHT trackpad, still"),
    ("RPAD-CLICK", "physically CLICK the RIGHT trackpad and hold"),
    (
        "LPAD-MOVE",
        "slowly CIRCLE the left thumb on the LEFT trackpad",
    ),
    (
        "RPAD-MOVE",
        "slowly CIRCLE the right thumb on the RIGHT trackpad",
    ),
];

fn open_probe_device() -> Option<HidDevice> {
    let api = HidApi::new().ok()?;

    // Dump EVERY Valve USB device first — the classic PID assumptions must be
    // verifiable from the output, not silent.
    let all: Vec<hidapi::DeviceInfo> = api
        .device_list()
        .filter(|d| d.vendor_id() == VENDOR_ID)
        .cloned()
        .collect();
    if all.is_empty() {
        println!("No Valve (28de:*) USB devices found at all.");
        println!("  - is the dongle plugged in / the controller paired via USB?");
        println!("  - is the controller AWAKE? (press the STEAM button once)");
        return None;
    }
    println!("Valve devices on USB:");
    for d in &all {
        let known = match d.product_id() {
            PRODUCT_ID_DONGLE => " <- classic SC wireless dongle (probe target)",
            PRODUCT_ID_WIRED_2015 => " <- classic SC wired (probe target)",
            crate::sc::PRODUCT_ID_PROTEUS => " <- SC 2026 dongle (Triton; already supported)",
            crate::sc::PRODUCT_ID_WIRED => " <- SC 2026 wired (Triton; already supported)",
            crate::sc::PRODUCT_ID_DECK => " <- USB-tethered Steam Deck (already supported)",
            _ => "",
        };
        println!(
            "  28de:{:04X} iface {:2} usage_page 0x{:04X} usage 0x{:02X} {}{}",
            d.product_id(),
            d.interface_number(),
            d.usage_page(),
            d.usage(),
            d.path().to_string_lossy(),
            known
        );
    }
    println!();

    // Try every classic-PID interface: open, listen 500 ms, report what
    // streams. Keep the first one streaming 0x01 frames for the phases.
    let classic: Vec<&hidapi::DeviceInfo> = all
        .iter()
        .filter(|d| d.product_id() == PRODUCT_ID_DONGLE || d.product_id() == PRODUCT_ID_WIRED_2015)
        .collect();
    if classic.is_empty() {
        println!("No 0x1102/0x1142 device enumerated — see the list above.");
        println!("If your unit shows a DIFFERENT 28de:xxxx PID, that's the fix:");
        println!("paste this output back and the tables get that PID added.");
        return None;
    }

    let mut chosen: Option<HidDevice> = None;
    for d in classic {
        let iface = d.interface_number();
        let dev = match api.open_path(d.path()) {
            Ok(dev) => dev,
            Err(e) => {
                println!("iface {iface}: open FAILED: {e} (permissions? try: sudo)");
                continue;
            }
        };
        let mut buf = [0u8; 64];
        match dev.read_timeout(&mut buf, 500) {
            Ok(0) => {
                println!(
                    "iface {iface}: opened, but SILENT for 500 ms \
                         (controller asleep? press STEAM to wake it)"
                );
            }
            Ok(n) => {
                println!(
                    "iface {iface}: streams report id 0x{:02X} ({n} bytes)",
                    buf[0]
                );
                if buf[0] == crate::sc::CLASSIC_INPUT_STATUS && chosen.is_none() {
                    chosen = Some(dev);
                }
            }
            Err(e) => println!("iface {iface}: read error: {e}"),
        }
    }

    if chosen.is_none() {
        println!("\nNo interface streamed 0x01 input frames — not running phases.");
        println!("Wake the controller (STEAM button) and re-run if it was silent.");
    }
    chosen
}

/// Frames OR-ed together over a capture window: bits set anywhere, plus
/// per-u16-offset min/max for axis discovery.
struct Capture {
    or_bits: [u8; 64],
    u16_min: [i16; 32],
    u16_max: [i16; 32],
    frames: usize,
}

impl Capture {
    fn new() -> Self {
        Self {
            or_bits: [0; 64],
            u16_min: [i16::MAX; 32],
            u16_max: [i16::MIN; 32],
            frames: 0,
        }
    }
}

fn capture(dev: &HidDevice, secs: f32) -> Capture {
    let mut cap = Capture::new();
    let deadline = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < deadline {
        let mut buf = [0u8; 64];
        match dev.read_timeout(&mut buf, 50) {
            Ok(0) => continue,
            Ok(n) if buf[0] == crate::sc::CLASSIC_INPUT_STATUS && n >= 20 => {}
            Ok(_) => continue,
            Err(_) => break,
        }
        cap.frames += 1;
        for i in 0..64 {
            cap.or_bits[i] |= buf[i];
        }
        for i in 0..32 {
            let v = i16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]);
            cap.u16_min[i] = cap.u16_min[i].min(v);
            cap.u16_max[i] = cap.u16_max[i].max(v);
        }
    }
    cap
}

fn diff_bits(base: &[u8; 64], held: &[u8; 64]) -> Vec<String> {
    let mut out = Vec::new();
    for byte in 0..64 {
        let d = held[byte] & !base[byte]; // bits that turned ON while held
        if d == 0 {
            continue;
        }
        for bit in 0..8 {
            if d & (1 << bit) != 0 {
                out.push(format!("byte {byte} bit {bit}"));
            }
        }
    }
    out
}

fn diff_axes(base: &Capture, held: &Capture) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..32 {
        let swing = |c: &Capture| c.u16_max[i].saturating_sub(c.u16_min[i]);
        // Compare against the baseline's own jitter, with a healthy margin.
        if swing(held) > swing(base).max(400) + 400 {
            out.push(format!(
                "u16@{}: {:+}..{:+} (baseline swing {})",
                i * 2,
                held.u16_min[i],
                held.u16_max[i],
                swing(base)
            ));
        }
    }
    out
}

pub fn probe() {
    println!("=== keebs classic Steam Controller probe ===");
    println!("This calibrates the PROVISIONAL wire tables in src/sc.rs.");
    println!("Follow the prompts; each phase runs automatically on a timer.\n");

    let Some(dev) = open_probe_device() else {
        return;
    };

    let mut prev: Option<Capture> = None;
    for (name, hint) in PROBE_PHASES {
        println!("--- {name}: {hint}");
        println!("    3 s to get ready…");
        std::thread::sleep(Duration::from_secs(3));
        println!("    capturing 2.5 s — HOLD IT NOW");
        let cap = capture(&dev, 2.5);
        println!("    got {} frames", cap.frames);
        if *name == "REST" {
            println!("    baseline hex (first 32 B):");
            for row in 0..4 {
                let hex: Vec<String> = cap.or_bits[row * 8..row * 8 + 8]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                println!("      {:2}: {}", row * 8, hex.join(" "));
            }
            prev = Some(cap);
            continue;
        }
        let base = prev.as_ref().expect("REST ran first");
        let bits = diff_bits(&base.or_bits, &cap.or_bits);
        let axes = diff_axes(base, &cap);
        println!(
            "    bits that turned ON:  {}",
            if bits.is_empty() {
                "(none)".into()
            } else {
                bits.join(", ")
            }
        );
        println!(
            "    axes that swung:      {}",
            if axes.is_empty() {
                "(none)".into()
            } else {
                axes.join(", ")
            }
        );
    }

    println!("\nprobe done — paste this output back so the sc.rs tables can be");
    println!("corrected if anything disagrees with the provisional layout.");
}
