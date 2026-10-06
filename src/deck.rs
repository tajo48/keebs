//! Steam Deck (USB-tethered, VID 0x28DE PID 0x1205) input driver.
//!
//! Rust port of `linux/steamcontroller/deck.py`. A USB-C-tethered Deck
//! exposes three HID interfaces; the vendor data interface (iface 2) streams
//! 64-byte report-id-0x01 frames in the CLASSIC 2015 Steam Controller wire
//! format. Button/axis layout was calibrated empirically against live
//! hardware (calibrate_deck.py 1-6, 2026-10-05) — see [`crate::sc`] for the
//! byte/bit tables.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use crate::sc::*;
use crate::stop::StopFlag;

/// The firmware re-enables lizard on its OWN timer (observed: a successful
/// disable wears off within seconds). While we want it OFF we must re-assert
/// much faster than that timer, or the emulated mouse twitches through under
/// the keyboard. While ON a lazy refresh is plenty.
const LIZARD_REFRESH_ON: f32 = 2.0;
const LIZARD_REFRESH_OFF: f32 = 0.2;
const CENTER_FRAMES: usize = 30;

/// True while a USB-tethered Deck is enumerable (cheap HID list). Uses the
/// exact PID — the narrow query is what the probes verified.
pub fn deck_present() -> bool {
    match HidApi::new() {
        Ok(api) => api
            .device_list()
            .any(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID_DECK),
        Err(_) => false,
    }
}

pub type YieldCheck = Box<dyn Fn() -> bool + Send + Sync>;

pub struct DeckController {
    pub stop: Arc<StopFlag>,
    api: HidApi,
    inner: Mutex<Option<HidDevice>>,
    opened: AtomicBool,
    lizard_enabled: AtomicBool,
    should_yield: Option<YieldCheck>,
}

impl DeckController {
    /// `passive=true` keeps lizard ON (desktop mode — pads drive the system
    /// mouse, e.g. for a chord watcher). `should_yield`, when it returns
    /// true, breaks the read loop so a watcher can get out of the device's
    /// way the moment the OSK opens its own Deck handle (two lizard
    /// asserters flapping = bad).
    pub fn new(
        passive: bool,
        should_yield: Option<YieldCheck>,
        stop: Arc<StopFlag>,
    ) -> Result<Self, hidapi::HidError> {
        Ok(Self {
            api: HidApi::new()?,
            stop,
            inner: Mutex::new(None),
            opened: AtomicBool::new(false),
            lizard_enabled: AtomicBool::new(passive),
            should_yield,
        })
    }

    fn with_dev<R>(&self, f: impl FnOnce(&HidDevice) -> R) -> Option<R> {
        let guard = self.inner.lock().unwrap();
        guard.as_ref().map(f)
    }

    fn send_lizard(&self, dev: &HidDevice) {
        let report = if self.lizard_enabled.load(Ordering::SeqCst) {
            &ENABLE_LIZARD_REPORT
        } else {
            &DISABLE_LIZARD_REPORT
        };
        let _ = dev.send_feature_report(report);
    }

    /// Open the vendor data interface — the one streaming 64-byte 0x01
    /// frames (verified by probing each Deck HID handle once).
    fn open_streaming_interface(&self) -> Option<HidDevice> {
        for info in self.api.device_list() {
            if info.vendor_id() != VENDOR_ID || info.product_id() != PRODUCT_ID_DECK {
                continue;
            }
            let path = info.path().to_owned();
            let Ok(dev) = self.api.open_path(&path) else {
                continue;
            };
            // Devices open in blocking mode by default — what we want.
            let mut buf = [0u8; 64];
            let good = matches!(dev.read_timeout(&mut buf, 300), Ok(n) if n == DECK_INPUT_REPORT_LEN && buf[0] == DECK_INPUT_REPORT_ID);
            if good {
                return Some(dev);
            }
        }
        None
    }

    // -- control ------------------------------------------------------------

    pub fn set_lizard(&self, enabled: bool) {
        self.lizard_enabled.store(enabled, Ordering::SeqCst);
        if self.inner.lock().unwrap().is_none() {
            return;
        }
        let _ = self.with_dev(|dev| self.send_lizard(dev));
        if !enabled {
            // Double-tap the disable: the firmware's own re-enable timer
            // races ours, and two sends back-to-back reliably win the
            // OSK-open transition (a 50 ms pause costs nothing once).
            thread::sleep(Duration::from_millis(50));
            let _ = self.with_dev(|dev| self.send_lizard(dev));
        }
    }

    /// `addExit()` equivalent.
    pub fn add_exit(&self) {
        self.stop.signal();
    }

    // -- haptics (best-effort; unverified on Deck firmware) ------------------

    /// Deliver one haptic report over every plausible channel: an OUTPUT
    /// write (what the 2015 SC honored), a 64-byte OUTPUT write (unnumbered
    /// variant), and a FEATURE set (the control pipe the Deck accepts). A
    /// duplicate is benign (same tone restarted), so try them all.
    fn send_haptic(&self, dev: &HidDevice, report: &[u8; 65]) {
        let _ = dev.write(report);
        let _ = dev.write(&report[..64]);
        let _ = dev.send_feature_report(report);
    }

    pub fn haptic_tone(&self, actuator: u8, freq_hz: u32, gain: i8, count: u16) -> bool {
        let report = build_haptic_tone_report(actuator, freq_hz, gain, count);
        self.with_dev(|dev| self.send_haptic(dev, &report))
            .is_some()
    }

    /// Tone-first click on both pad actuators, no preceding stop: a 0x82 stop
    /// the firmware doesn't recognize must not wedge the pipe before the tone.
    pub fn haptic_click(&self, freq_hz: u32, gain: i8, count: u16) -> bool {
        let mut ok = false;
        for act in [HAPTIC_PAD_LEFT, HAPTIC_PAD_RIGHT] {
            ok = self.haptic_tone(act, freq_hz, gain, count) || ok;
        }
        ok
    }

    pub fn haptic_pad_click(&self) {
        for act in [HAPTIC_PAD_LEFT, HAPTIC_PAD_RIGHT] {
            let report = build_haptic_tone_report(act, 300, HAPTIC_PAD_CLICK_GAIN, 6);
            let _ = self.with_dev(|dev| self.send_haptic(dev, &report));
        }
    }

    // -- main loop ----------------------------------------------------------

    /// Blocks while the Deck streams input, invoking `cb` per frame.
    pub fn run(self: &Arc<Self>, cb: &mut dyn FnMut(&Arc<Self>, &SciInput)) {
        // Open with a fast internal retry (~100 ms steps, up to 2 s): when a
        // watcher hands the Deck over to the OSK, closing its hidraw handle
        // makes the device briefly vanish from enumeration. Retrying absorbs
        // that ~200 ms blip invisibly instead of a multi-second offline gap.
        let deadline = Instant::now() + Duration::from_secs(2);
        let dev = loop {
            if let Some(dev) = self.open_streaming_interface() {
                break dev;
            }
            if self.stop.is_set() {
                return;
            }
            if let Some(f) = &self.should_yield {
                if f() {
                    return;
                }
            }
            if Instant::now() >= deadline {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        };
        *self.inner.lock().unwrap() = Some(dev);
        self.opened.store(true, Ordering::SeqCst);
        let _ = self.with_dev(|dev| self.send_lizard(dev));

        // Auto-center the sticks: average the first quiet frames. Raw rest
        // offsets were observed up to ~900 on hardware, so this matters. 30
        // frames arrive in ~120 ms at the Deck's ~250 Hz rate; the deadline
        // only guards a silent device. Keep it SHORT — this window is dead
        // time at every OSK open.
        let mut sums = [0i32; 4];
        let mut n = 0usize;
        let deadline = Instant::now() + Duration::from_millis(150);
        while n < CENTER_FRAMES && Instant::now() < deadline {
            let frame = self.with_dev(|dev| {
                let mut buf = [0u8; 64];
                match dev.read_timeout(&mut buf, 50) {
                    Ok(m) if m > 0 => Some(buf[..m].to_vec()),
                    _ => None,
                }
            });
            let Some(Some(data)) = frame else { continue };
            if let Some([lx, ly, rx, ry]) = deck_sticks(&data) {
                sums[0] += lx as i32;
                sums[1] += ly as i32;
                sums[2] += rx as i32;
                sums[3] += ry as i32;
                n += 1;
            }
        }
        let centers = if n > 0 {
            [
                (sums[0] / n as i32) as i16,
                (sums[1] / n as i32) as i16,
                (sums[2] / n as i32) as i16,
                (sums[3] / n as i32) as i16,
            ]
        } else {
            [0; 4]
        };

        let mut last_lizard = Instant::now();
        loop {
            if self.stop.is_set() {
                break;
            }
            if let Some(f) = &self.should_yield {
                if f() {
                    break;
                }
            }
            let refresh = if self.lizard_enabled.load(Ordering::SeqCst) {
                LIZARD_REFRESH_ON
            } else {
                LIZARD_REFRESH_OFF
            };
            if last_lizard.elapsed().as_secs_f32() > refresh {
                let _ = self.with_dev(|dev| self.send_lizard(dev));
                last_lizard = Instant::now();
            }

            let frame = self.with_dev(|dev| {
                let mut buf = [0u8; 64];
                match dev.read_timeout(&mut buf, 200) {
                    Ok(m) if m > 0 => Ok(Some(buf[..m].to_vec())),
                    Ok(_) => Ok(None),
                    Err(_) => Err(()), // device dropped
                }
            });
            match frame {
                None | Some(Err(())) => break,
                Some(Ok(None)) => continue,
                Some(Ok(Some(data))) => {
                    if let Some(sci) = parse_deck(&data, centers) {
                        cb(self, &sci);
                    }
                }
            }
        }

        // Teardown: restore lizard so the Deck works as a normal mouse/kb
        // the moment we let go of it.
        self.opened.store(false, Ordering::SeqCst);
        let dev = self.inner.lock().unwrap().take();
        if let Some(dev) = dev {
            self.lizard_enabled.store(true, Ordering::SeqCst);
            let _ = dev.send_feature_report(&ENABLE_LIZARD_REPORT);
        }
    }
}
