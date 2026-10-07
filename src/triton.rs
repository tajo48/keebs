//! Steam Controller driver targeting the "Triton" wireless adapter
//! (PID 0x1304) and the wired 2026 controller (PID 0x1302).
//!
//! Rust port of the `SteamController` class in
//! `linux/steamcontroller/__init__.py`. Protocol types/builders live in
//! [`crate::sc`]. Linux notes preserved from the Python driver:
//!
//! - We force the hidraw hidapi backend (see Cargo.toml): the libusb backend
//!   duplicates the report-ID byte in SET_REPORT payloads, which Triton
//!   firmware rejects.
//! - Lizard mode is NOT touched at runtime: on this hardware the
//!   SET_SETTINGS write apparently persists in firmware, and only a physical
//!   power-cycle restores defaults. Input reports flow alongside firmware
//!   kb/mouse emulation, so the watchdog thread exists only for teardown
//!   parity and does nothing.

use std::collections::HashSet;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use crate::diag;
use crate::sc::*;
use crate::stop::StopFlag;

const OPEN_RETRY_ATTEMPTS: u32 = 5;
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(100);

static LAST_GOOD_PATH: Mutex<Option<CString>> = Mutex::new(None);
static ABSENCE_NOTED: AtomicBool = AtomicBool::new(false);

struct Candidate {
    path: CString,
    pid: u16,
    iface: i32,
}

/// Vendor-specific HID data interfaces (usage page 0xFF00 usage 1, or a
/// hidraw-style 0/0) for both the dongle and the wired controller.
fn enumerate_data_interfaces(api: &HidApi) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for info in api.device_list() {
        if info.vendor_id() != VENDOR_ID {
            continue;
        }
        let pid = info.product_id();
        if pid != PRODUCT_ID_PROTEUS && pid != PRODUCT_ID_WIRED {
            continue;
        }
        let (up, us) = (info.usage_page(), info.usage());
        if (up == 0xFF00 && us == 1) || (up == 0 && us == 0) {
            out.push(Candidate {
                path: info.path().to_owned(),
                pid,
                iface: info.interface_number(),
            });
        }
    }
    out.sort_by_key(|c| (c.pid, c.iface));
    out
}

/// True while any Triton device is enumerable (cheap HID list, no open).
pub fn triton_present() -> bool {
    match HidApi::new() {
        Ok(api) => api.device_list().any(|d| {
            d.vendor_id() == VENDOR_ID
                && (d.product_id() == PRODUCT_ID_PROTEUS || d.product_id() == PRODUCT_ID_WIRED)
        }),
        Err(_) => false,
    }
}

pub struct SteamController {
    pub stop: Arc<StopFlag>,
    api: HidApi,
    inner: Mutex<Option<HidDevice>>,
    opened: AtomicBool,
    passive: bool,
    lizard_enabled: AtomicBool,
    battery: Mutex<Option<(Battery, Instant)>>,
    rumble_state: Mutex<(u8, u8)>,
    rumble_thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl SteamController {
    /// `passive=true` keeps firmware kb/mouse (lizard) ON — the desktop-mode /
    /// chord-watcher posture. `false` (OSK / gamepad mode) sends
    /// DISABLE_LIZARD at open time.
    pub fn new(passive: bool, stop: Arc<StopFlag>) -> Result<Self, HidError> {
        Ok(Self {
            api: HidApi::new()?,
            stop,
            inner: Mutex::new(None),
            opened: AtomicBool::new(false),
            passive,
            lizard_enabled: AtomicBool::new(passive),
            battery: Mutex::new(None),
            rumble_state: Mutex::new((0, 0)),
            rumble_thread: Mutex::new(None),
        })
    }

    fn with_dev<R>(&self, f: impl FnOnce(&HidDevice) -> R) -> Option<R> {
        let guard = self.inner.lock().unwrap();
        guard.as_ref().map(f)
    }

    fn open_device(&self, path: &CString) -> Result<HidDevice, String> {
        let mut last_err = String::from("no attempt made");
        for attempt in 0..OPEN_RETRY_ATTEMPTS {
            match self.api.open_path(path) {
                Ok(dev) => return Ok(dev),
                Err(e) => {
                    // A block toggle reopens right after closing our own
                    // handle; the OS can briefly race us in either direction.
                    // Retries only fire on failure, so cold opens pay nothing.
                    last_err = e.to_string();
                    if attempt + 1 < OPEN_RETRY_ATTEMPTS {
                        thread::sleep(OPEN_RETRY_DELAY);
                    }
                }
            }
        }
        Err(last_err)
    }

    fn open_first_responsive(&self) -> Result<(), String> {
        let mut candidates = enumerate_data_interfaces(&self.api);
        if candidates.is_empty() {
            return Err(format!(
                "No Steam Controller 2026 interface found (VID 0x{VENDOR_ID:04X}, \
                 PID 0x{PRODUCT_ID_PROTEUS:04X} dongle / 0x{PRODUCT_ID_WIRED:04X} wired)."
            ));
        }

        // Try the last-known-good interface first: a rebuild (gamepad<->lizard
        // switch on alt-tab) then comes live in milliseconds instead of
        // probing every dongle slot for up to 1.5 s.
        let last_good = LAST_GOOD_PATH.lock().unwrap().clone();
        if let Some(lg) = last_good {
            candidates.sort_by_key(|c| c.path != lg);
        }

        let mut last_err: Option<String> = None;
        for cand in candidates {
            let dev = match self.open_device(&cand.path) {
                Ok(d) => d,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };

            if !self.passive {
                match dev.send_feature_report(&DISABLE_LIZARD_REPORT) {
                    Ok(_) => println!("steamcontroller: disable-lizard on iface {} ok", cand.iface),
                    Err(e) => {
                        last_err = Some(e.to_string());
                        continue;
                    }
                }
            }

            // Probe & VALIDATE. The dongle exposes several slots and an
            // unpaired/phantom one can emit input-shaped junk forever — the
            // first-responsive slot is NOT trustworthy (observed on live
            // hardware: phantom key dispatches + never-quiet button flapping
            // that locks every other controller out of the OSK). Accept only
            // a slot that streams input frames AND shows at least one fully
            // quiet (hands-off) frame in the window. If the user happens to
            // be holding buttons during validation, the reader retries in 2 s
            // and succeeds once the controller is at rest.
            let deadline = Instant::now() + Duration::from_millis(400);
            let (mut frames, mut quiet) = (0usize, 0usize);
            let mut prev_buttons: Option<u32> = None;
            let mut flaps = 0usize;
            while Instant::now() < deadline {
                let mut buf = [0u8; 64];
                let n = match dev.read_timeout(&mut buf, 100) {
                    Ok(n)
                        if n >= TRITON_INPUT_MIN_LEN
                            && TRITON_INPUT_REPORT_IDS.contains(&buf[0]) =>
                    {
                        n
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        last_err = Some(e.to_string());
                        break;
                    }
                };
                let Some(sci) = parse_triton(&buf[..n]) else {
                    continue;
                };
                frames += 1;
                let mask = sci.buttons & !(buttons::LGRIP_REST | buttons::RGRIP_REST);
                if mask == 0 {
                    quiet += 1;
                }
                if prev_buttons.is_some_and(|p| p != sci.buttons) {
                    flaps += 1;
                }
                prev_buttons = Some(sci.buttons);
            }

            let ok = frames >= 2 && quiet >= 1;
            let verdict = if ok {
                "accepted"
            } else if frames == 0 {
                "rejected (silent — controller asleep or unpaired?)"
            } else {
                "rejected (streams but never quiet — junk slot?)"
            };
            diag::log(&format!(
                "steamcontroller: iface {} probe: {frames} frames, {quiet} quiet, {flaps} button flaps -> {verdict}",
                cand.iface
            ));
            if ok {
                *self.inner.lock().unwrap() = Some(dev);
                *LAST_GOOD_PATH.lock().unwrap() = Some(cand.path.clone());
                diag::log(&format!("steamcontroller: opened iface {}", cand.iface));
                return Ok(());
            }
        }

        Err(format!(
            "Found Steam Controller 2026 interfaces but none passed validation \
             (need input frames + at least one hands-off quiet frame; a phantom \
             dongle slot streams junk that never quiets). Last error: {last_err:?}"
        ))
    }

    /// Re-assert the desired lizard state every LIZARD_REFRESH_SECONDS.
    /// On Linux this thread intentionally does nothing: see the module docs.
    fn spawn_lizard_watchdog(&self) {
        let stop = Arc::clone(&self.stop);
        thread::spawn(move || while !stop.wait(Duration::from_secs(3600)) {});
    }

    fn note_frame(&self, batt: Option<Battery>) {
        let now = Instant::now();
        let mut guard = self.battery.lock().unwrap();
        match batt {
            Some(b) => *guard = Some((b, now)),
            None => {
                if let Some(entry) = guard.as_mut() {
                    entry.1 = now;
                }
            }
        }
    }

    // -- control ------------------------------------------------------------

    /// Toggle lizard (firmware mouse/kb) at runtime. The hardware watchdog
    /// re-asserts in 3-5 s, so longer suppression needs periodic re-sends.
    pub fn set_lizard(&self, enabled: bool) {
        self.lizard_enabled.store(enabled, Ordering::SeqCst);
        let report = if enabled {
            &ENABLE_LIZARD_REPORT
        } else {
            &DISABLE_LIZARD_REPORT
        };
        let sent = self.with_dev(|dev| dev.send_feature_report(report));
        if sent.is_none() {
            return;
        }
        if !enabled {
            // Double-tap the disable so the OSK-open transition wins the race
            // against the firmware's own re-enable timer.
            thread::sleep(Duration::from_millis(50));
            let _ = self.with_dev(|dev| dev.send_feature_report(report));
        }
    }

    /// Ask the controller to power itself off. Known not to work on
    /// Triton/puck hardware over Linux transports — hold Steam ~10 s instead.
    pub fn turn_off(&self) -> bool {
        let sent = self.with_dev(|dev| dev.send_feature_report(&TURN_OFF_REPORT));
        match sent {
            Some(Ok(())) => {
                println!(
                    "steamcontroller: sent turn-off command \
                     (firmware likely ignores on Triton; hold Steam ~10s)"
                );
                true
            }
            Some(Err(e)) => {
                println!("steamcontroller: turn_off failed: {e}");
                false
            }
            None => false,
        }
    }

    // -- haptics ------------------------------------------------------------

    /// Play an LFO tone on one actuator (0x83). Small `count` = short burst.
    pub fn haptic_tone(&self, actuator: u8, freq_hz: u32, gain: i8, count: u16) -> bool {
        let report = build_haptic_tone_report(actuator, freq_hz, gain, count);
        match self.with_dev(|dev| dev.write(&report)) {
            Some(Ok(_)) => true,
            Some(Err(e)) => {
                println!("steamcontroller: haptic_tone failed: {e}");
                false
            }
            None => false,
        }
    }

    /// Stop the tone on one actuator (0x82).
    pub fn haptic_stop(&self, actuator: u8) -> bool {
        let report = build_haptic_stop_report(actuator);
        match self.with_dev(|dev| dev.write(&report)) {
            Some(Ok(_)) => true,
            Some(Err(e)) => {
                println!("steamcontroller: haptic_stop failed: {e}");
                false
            }
            None => false,
        }
    }

    /// Crisp trackpad click for UI feedback: a very short burst on both pads,
    /// plus a timed safety-stop in case the hardware ignores the burst count.
    pub fn haptic_click(self: &Arc<Self>, freq_hz: u32, gain: i8, count: u16, duration_ms: u64) {
        let wrote = self.with_dev(|dev| {
            for act in [HAPTIC_PAD_LEFT, HAPTIC_PAD_RIGHT] {
                let report = build_haptic_tone_report(act, freq_hz, gain, count);
                let _ = dev.write(&report);
            }
        });
        if wrote.is_none() {
            return;
        }
        let weak = Arc::downgrade(self);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(duration_ms));
            if let Some(sc) = weak.upgrade() {
                for act in [HAPTIC_PAD_LEFT, HAPTIC_PAD_RIGHT] {
                    sc.haptic_stop(act);
                }
            }
        });
    }

    /// 'Physical pad click' tick: a short, crisp, slightly firmer pop than
    /// the light key tap.
    pub fn haptic_pad_click(self: &Arc<Self>) {
        self.haptic_click(HAPTIC_PAD_CLICK_FREQ, HAPTIC_PAD_CLICK_GAIN, 4, 14);
    }

    /// Short rising (on) / falling (off) two-tone chime on both pads with a
    /// soft low-D body pedal. Blocks ~0.35 s; call from a worker thread and
    /// BEFORE device teardown or the trailing stops cut it short.
    pub fn play_chime(&self, on: bool) {
        let notes = if on {
            CHIME_NOTES
        } else {
            [CHIME_NOTES[1], CHIME_NOTES[0]]
        };
        for (i, freq) in notes.iter().enumerate() {
            let voiced = self.with_dev(|dev| {
                for (act, f, gain) in [
                    (HAPTIC_PAD_RIGHT, *freq, HAPTIC_CHIME_GAIN),
                    (HAPTIC_PAD_LEFT, *freq + CHIME_DETUNE_HZ, HAPTIC_CHIME_GAIN),
                    (CHIME_BODY_ACTUATOR, CHIME_BODY_FREQ, CHIME_BODY_GAIN),
                ] {
                    // Stop before each tone for a clean onset (also required
                    // on the motor — omitting it there can reboot the unit).
                    let _ = dev.write(&build_haptic_stop_report(act));
                    let _ = dev.write(&build_haptic_tone_report(
                        act,
                        f,
                        gain,
                        tone_count(f, CHIME_TONE_SECONDS),
                    ));
                }
            });
            if voiced.is_none() {
                return;
            }
            thread::sleep(Duration::from_secs_f32(CHIME_DURATIONS[i]));
        }
        self.with_dev(|dev| {
            for act in [HAPTIC_PAD_RIGHT, HAPTIC_PAD_LEFT, CHIME_BODY_ACTUATOR] {
                let _ = dev.write(&build_haptic_stop_report(act));
            }
        });
    }

    // -- rumble -------------------------------------------------------------

    /// Write the two back-motor tones as SHORT self-expiring bursts,
    /// re-validated against the latest requested state so a stale keepalive
    /// re-arm can't clobber a just-stopped motor.
    fn emit_rumble(&self, large: u8, small: u8) -> bool {
        self.with_dev(|dev| {
            {
                let state = self.rumble_state.lock().unwrap();
                if *state != (large, small) {
                    // A newer request landed; let its own emit win.
                    return false;
                }
            }
            for (act, intensity, freq) in [
                (HAPTIC_RUMBLE_LEFT, large, RUMBLE_FREQ_LOW),
                (HAPTIC_RUMBLE_RIGHT, small, RUMBLE_FREQ_HIGH),
            ] {
                let _ = dev.write(&build_haptic_stop_report(act));
                if intensity > 0 {
                    let _ = dev.write(&build_haptic_tone_report(
                        act,
                        freq,
                        rumble_gain(intensity as u32),
                        tone_count(freq, RUMBLE_TONE_SECONDS),
                    ));
                }
            }
            true
        })
        .unwrap_or(false)
    }

    /// Drive the back rumble motors from XInput intensities (0..255); 0 stops.
    pub fn set_rumble(self: &Arc<Self>, large: u8, small: u8) -> bool {
        *self.rumble_state.lock().unwrap() = (large, small);
        let ok = self.emit_rumble(large, small);
        if large != 0 || small != 0 {
            self.ensure_rumble_keepalive();
        }
        ok
    }

    fn ensure_rumble_keepalive(self: &Arc<Self>) {
        let mut handle = self.rumble_thread.lock().unwrap();
        if let Some(h) = handle.as_ref() {
            if !h.is_finished() {
                return;
            }
        }
        let weak = Arc::downgrade(self);
        *handle = Some(thread::spawn(move || {
            // If this process dies before set_rumble(0,0), the last bounded
            // burst simply lapses on its own — never an infinite buzz.
            while let Some(sc) = weak.upgrade() {
                let (large, small) = *sc.rumble_state.lock().unwrap();
                if (large == 0 && small == 0) || sc.stop.is_set() {
                    break;
                }
                sc.emit_rumble(large, small);
                if sc
                    .stop
                    .wait(Duration::from_secs(RUMBLE_REFRESH_SECONDS as u64))
                {
                    break;
                }
            }
        }));
    }

    // -- status -------------------------------------------------------------

    /// Latest battery reading, or `None` if none streamed this session or the
    /// controller went silent (powered off / dropped its wireless link).
    pub fn get_battery(&self) -> Option<Battery> {
        let guard = self.battery.lock().unwrap();
        let (batt, at) = guard.as_ref()?;
        if at.elapsed() > Duration::from_secs_f32(BATTERY_FRESH_SECONDS) {
            return None;
        }
        Some(*batt)
    }

    /// True once the device is open and usable.
    pub fn is_live(&self) -> bool {
        self.opened.load(Ordering::SeqCst) && self.inner.lock().unwrap().is_some()
    }

    /// `addExit()` equivalent: break the read loop and tear down.
    pub fn add_exit(&self) {
        self.stop.signal();
    }

    // -- main loop ----------------------------------------------------------

    /// Blocks while the controller streams input, invoking `cb` per frame.
    /// Returns when the controller disappears or [`Self::add_exit`] fires.
    pub fn run(self: &Arc<Self>, cb: &mut dyn FnMut(&Arc<Self>, &SciInput)) {
        // Presence gate: never attempt the open when no Triton device is even
        // enumerable — callers re-invoke on a short reconnect cadence, and a
        // machine with only, say, a tethered Deck (a different device) would
        // otherwise log open failures forever.
        let pids: HashSet<u16> = present_product_ids(&self.api);
        if !(pids.contains(&PRODUCT_ID_PROTEUS) || pids.contains(&PRODUCT_ID_WIRED)) {
            if !ABSENCE_NOTED.swap(true, Ordering::SeqCst) {
                diag::log(
                    "steamcontroller: no Steam Controller 2026 present \
                     (looking for PID 0x{PRODUCT_ID_PROTEUS:04X} dongle / \
                     0x{PRODUCT_ID_WIRED:04X} wired; other Valve USB devices \
                     such as a Steam Deck are not supported by this driver)"
                        .replace(
                            "{PRODUCT_ID_PROTEUS:04X}",
                            &format!("{:04X}", PRODUCT_ID_PROTEUS),
                        )
                        .replace(
                            "{PRODUCT_ID_WIRED:04X}",
                            &format!("{:04X}", PRODUCT_ID_WIRED),
                        )
                        .as_str(),
                );
            }
            return;
        }
        ABSENCE_NOTED.store(false, Ordering::SeqCst);

        if let Err(e) = self.open_first_responsive() {
            diag::log(&format!("steamcontroller: open failed: {e}"));
            return;
        }
        self.opened.store(true, Ordering::SeqCst);

        // Rest normalization (the Deck driver's hard-won lesson, applied to
        // the 2026 controller): average the first quiet frames so sticks and
        // triggers read ~0 at rest. Without this, a resting stick beyond the
        // mouse deadzone drifts the cursor forever and a resting trigger
        // past 16384 permanently latches the Shift/Enter roles. Pads are
        // absolute-position and are NOT normalized.
        let mut sums = [0i32; 6];
        let mut n = 0usize;
        let deadline = Instant::now() + Duration::from_millis(150);
        while n < 30 && Instant::now() < deadline {
            let mut buf = [0u8; 64];
            let got = {
                let guard = self.inner.lock().unwrap();
                let Some(dev) = guard.as_ref() else { break };
                dev.read_timeout(&mut buf, 50).unwrap_or(0)
            };
            if got == 0 {
                continue;
            }
            if let Some(sci) = parse_triton(&buf[..got]) {
                sums[0] += i32::from(sci.lstick_x);
                sums[1] += i32::from(sci.lstick_y);
                sums[2] += i32::from(sci.rstick_x);
                sums[3] += i32::from(sci.rstick_y);
                sums[4] += i32::from(sci.ltrig);
                sums[5] += i32::from(sci.rtrig);
                n += 1;
            }
        }
        let centers = if n > 0 {
            sums.map(|s| (s / n as i32) as i16)
        } else {
            [0i16; 6]
        };

        if !self.passive {
            self.spawn_lizard_watchdog();
        }

        loop {
            if self.stop.is_set() {
                break;
            }
            let mut buf = [0u8; 64];
            // The lock is held across the read syscall (<= 200 ms), so
            // set_lizard/haptics from other threads briefly wait — same
            // effect as the Python single-threaded read loop.
            let n = {
                let guard = self.inner.lock().unwrap();
                let Some(dev) = guard.as_ref() else { break };
                match dev.read_timeout(&mut buf, 200) {
                    Ok(n) => n,
                    Err(e) => {
                        println!("steamcontroller: read error: {e}");
                        break;
                    }
                }
            };
            if n == 0 {
                continue;
            }

            // Power-status and link-status reports interleave with input;
            // pull battery out here and drop link frames.
            match buf[0] {
                TRITON_BATTERY_REPORT_ID => {
                    self.note_frame(parse_battery(&buf[..n]));
                    continue;
                }
                0x46 | 0x79 => continue,
                _ => {}
            }

            if let Some(mut sci) = parse_triton(&buf[..n]) {
                sci.lstick_x -= centers[0];
                sci.lstick_y -= centers[1];
                sci.rstick_x -= centers[2];
                sci.rstick_y -= centers[3];
                // Triggers only pull one way: clamp the rest-corrected value
                // at zero so a negative rest offset can't read as "pulled".
                sci.ltrig = (sci.ltrig - centers[4]).max(0);
                sci.rtrig = (sci.rtrig - centers[5]).max(0);
                self.note_frame(None);
                cb(self, &sci);
            }
        }

        self.teardown();
    }

    fn teardown(&self) {
        self.opened.store(false, Ordering::SeqCst);
        let dev = self.inner.lock().unwrap().take();
        if let Some(dev) = dev {
            // Stop any haptics still playing so the controller doesn't keep
            // buzzing after we release the device.
            for act in [
                HAPTIC_PAD_LEFT,
                HAPTIC_PAD_RIGHT,
                HAPTIC_RUMBLE_LEFT,
                HAPTIC_RUMBLE_RIGHT,
            ] {
                let _ = dev.write(&build_haptic_stop_report(act));
            }
            // Restore lizard mode immediately instead of waiting for the
            // hardware watchdog (~3-5 s).
            if !self.passive {
                let _ = dev.send_feature_report(&ENABLE_LIZARD_REPORT);
            }
        }
    }
}

type HidError = hidapi::HidError;
