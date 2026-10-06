//! Steam Deck (USB-tethered, VID 0x28DE PID 0x1205) input driver.
//!
//! Rust port of `steamcontroller/deck.py`. Everything goes through ONE
//! libusb session on the vendor data interface (iface 2) — interrupt IN for
//! the 64-byte 0x01 input frames, class SET_REPORT control transfers for
//! lizard mode, interrupt OUT for haptics — exactly like the pip hidapi
//! *libusb* backend the Python driver ran on. Mixing hidraw reads with a
//! libusb claim does NOT work: detaching usbhid destroys the hidraw node.
//!
//! Button/axis layout was calibrated empirically (calibrate_deck.py 1-6,
//! 2026-10-05) — see [`crate::sc`] for the byte/bit tables.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::lizardguard::LizardGrabber;
use crate::sc::*;
use crate::stop::StopFlag;
use crate::usb::LibusbSession;

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
    match HidApiDevice::new() {
        Ok(api) => api
            .device_list()
            .any(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID_DECK),
        Err(_) => false,
    }
}

// Thin alias so deck_present doesn't drag the hidapi crate name around.
use hidapi as hidapi_crate;
type HidApiDevice = hidapi_crate::HidApi;

pub type YieldCheck = Box<dyn Fn() -> bool + Send + Sync>;

enum ReadOutcome {
    /// `n` bytes in the frame buffer.
    Data(usize),
    /// Quiet for the whole timeout.
    Quiet,
    /// Device gone — bail out of the loop.
    Gone,
}

pub struct DeckController {
    pub stop: Arc<StopFlag>,
    opened: AtomicBool,
    lizard_enabled: AtomicBool,
    should_yield: Option<YieldCheck>,
    session: Mutex<Option<LibusbSession>>,
    /// Kernel-level lizard "off": EVIOCGRAB on the Deck's evdev nodes.
    /// Lizard stays ENABLED at the firmware level on purpose — disabling it
    /// kills the firmware's native trackpad haptics (the "they just happen"
    /// feedback the Python app had). The grab swallows the firmware's
    /// mouse/keyboard output instead.
    grabber: Mutex<LizardGrabber>,
}

impl DeckController {
    /// `passive=true` keeps lizard ON (desktop mode); `false` (OSK owning the
    /// controller) disables firmware kb/mouse at open and keeps re-asserting.
    pub fn new(
        passive: bool,
        should_yield: Option<YieldCheck>,
        stop: Arc<StopFlag>,
    ) -> Result<Self, ()> {
        let flag = AtomicBool::new(passive);
        Ok(Self {
            stop,
            opened: AtomicBool::new(false),
            lizard_enabled: flag,
            should_yield,
            session: Mutex::new(None),
            grabber: Mutex::new(LizardGrabber::default()),
        })
    }

    /// Read one frame into `buf`, holding the session lock only for the
    /// syscall (callers must drop the outcome before invoking callbacks,
    /// which may re-lock for lizard refreshes).
    fn read_frame(&self, buf: &mut [u8; 64], timeout_ms: u32) -> ReadOutcome {
        let guard = self.session.lock().unwrap();
        match guard.as_ref() {
            None => ReadOutcome::Gone,
            Some(s) => match s.read_timeout(buf, timeout_ms) {
                Ok(0) => ReadOutcome::Quiet,
                Ok(n) => ReadOutcome::Data(n),
                Err(()) => ReadOutcome::Gone,
            },
        }
    }

    // (lizard SET_REPORT sends removed — see the grabber note on the struct:
    // disabling lizard via feature reports kills the firmware's native
    // haptics, which is exactly the feedback we want to keep alive.)

    // -- control ------------------------------------------------------------

    /// Lizard "toggle" — now implemented as grab/release, NOT a firmware
    /// write. `enabled=true` (desktop mode) releases the grab so the pads
    /// drive the desktop mouse again; `false` (OSK owns the controller)
    /// grabs the nodes so firmware output is swallowed while its native
    /// haptics keep firing.
    pub fn set_lizard(&self, enabled: bool) {
        self.lizard_enabled.store(enabled, Ordering::SeqCst);
        let mut grabber = self.grabber.lock().unwrap();
        if enabled {
            grabber.release();
        } else {
            grabber.engage();
        }
    }

    /// `addExit()` equivalent.
    pub fn add_exit(&self) {
        self.stop.signal();
    }

    // -- haptics (best-effort; unverified on Deck firmware) ------------------

    fn write_report(&self, report: &[u8; 65]) -> bool {
        let guard = self.session.lock().unwrap();
        let Some(s) = guard.as_ref() else {
            return false;
        };
        // The Python deck driver's triple-channel `_send_haptic`, plus the
        // wire shapes its pip-hidapi libusb backend actually put on the wire:
        //   1. interrupt OUT, 65 B (id included)
        //   2. interrupt OUT, 64 B (unnumbered variant)
        //   3. SET_REPORT OUTPUT, clean 64 B body
        //   4. SET_REPORT OUTPUT, 65 B payload with the ID duplicated
        //      (hidapi-libusb's quirk — the shape lizard disable rode)
        //   5. SET_REPORT FEATURE, 65 B payload with the ID duplicated
        // A duplicate is benign (same tone restarted) — we can't detect
        // which channel the firmware honors, so try them all, Python-style.
        let r1 = s.write(report);
        let r2 = s.write(&report[..64]);
        let r3 = s.control_output(report[0], &report[1..]);
        let r4 = s.control_output_dup(report[0], report);
        let r5 = s.control_feature_dup(report[0], report);
        {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                println!(
                    "deck: haptic write channels int65={r1} int64={r2} out64={r3} outDup65={r4} featDup65={r5}"
                );
            });
        }
        r1 || r2 || r3 || r4 || r5
    }

    pub fn haptic_tone(&self, actuator: u8, freq_hz: u32, gain: i8, count: u16) -> bool {
        let report = build_haptic_tone_report(actuator, freq_hz, gain, count);
        self.write_report(&report)
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
            self.write_report(&report);
        }
    }

    /// Classic-protocol haptics — the format the mainline `hid-steam` kernel
    /// driver uses for the original Steam Controller (which the tethered
    /// Deck emulates): feature report 0x8F carrying a dual-motor
    /// vibration word pair. Sent over the same SET_REPORT channel that
    /// demonstrably reaches the firmware (it's what disables lizard).
    /// word1/word2 are 16-bit motor intensities (hid-steam rumble words).
    pub fn rumble(&self, word1: u16, word2: u16) -> bool {
        let payload: [u8; 10] = [
            0x07, // length field (hid-steam: 0x07)
            0x07, // haptic command
            0x00, // pad/mode: both
            (word1 & 0xFF) as u8,
            (word1 >> 8) as u8,
            (word2 & 0xFF) as u8,
            (word2 >> 8) as u8,
            0x00,
            0x00,
            0x00,
        ];
        let guard = self.session.lock().unwrap();
        match guard.as_ref() {
            Some(s) => s.send_feature_payload(0x8F, &payload),
            None => false,
        }
    }

    /// One-shot diagnostic (debug rounds): fire several candidate encodings
    /// of the classic 0x8F haptic report, labeled, so a physical buzz maps
    /// directly to the right byte layout. Currently unused — kept for the
    /// next reverse-engineering round if the pip-hidapi wire shapes fail.
    #[allow(dead_code)]
    fn haptic_probe(&self) {
        thread::sleep(Duration::from_millis(300));
        let variants: [(&str, [u8; 10]); 4] = [
            (
                "A dual-words@4",
                [0x07, 0x07, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00],
            ),
            (
                "B dual-words@3",
                [0x07, 0x07, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00],
            ),
            (
                "C pulse pad0",
                [0x07, 0x07, 0x00, 0x01, 0xFF, 0xFF, 0x10, 0x27, 0x08, 0x00],
            ),
            (
                "D pulse pad1",
                [0x07, 0x07, 0x01, 0x01, 0xFF, 0xFF, 0x10, 0x27, 0x08, 0x00],
            ),
        ];
        for (name, payload) in variants {
            let guard = self.session.lock().unwrap();
            let sent = guard
                .as_ref()
                .map(|s| s.send_feature_payload(0x8F, &payload))
                .unwrap_or(false);
            drop(guard);
            println!("deck: haptic probe {name} sent={sent} — buzz?");
            thread::sleep(Duration::from_millis(600));
        }
        // E: the full 11-byte hid-steam report over interrupt OUT instead.
        let report11 = [
            0x8F, 0x07, 0x07, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00,
        ];
        let guard = self.session.lock().unwrap();
        let sent = guard.as_ref().map(|s| s.write(&report11)).unwrap_or(false);
        println!("deck: haptic probe E out-interrupt sent={sent} — buzz?");
    }

    // -- main loop ----------------------------------------------------------

    /// Blocks while the Deck streams input, invoking `cb` per frame.
    pub fn run(self: &Arc<Self>, cb: &mut dyn FnMut(&Arc<Self>, &SciInput)) {
        // Open with a fast internal retry (~100 ms steps, up to 2 s): after a
        // handoff the device can briefly vanish from enumeration.
        let deadline = Instant::now() + Duration::from_secs(2);
        let session = loop {
            match LibusbSession::open(VENDOR_ID, PRODUCT_ID_DECK, 2) {
                Some(s) => break s,
                None => {}
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
                println!("deck: could not open libusb session on iface 2");
                return;
            }
            thread::sleep(Duration::from_millis(100));
        };
        *self.session.lock().unwrap() = Some(session);
        self.opened.store(true, Ordering::SeqCst);
        // NOTE: no lizard SET_REPORT writes — lizard stays ENABLED at the
        // firmware level so its native trackpad haptics keep working; the
        // grabber (driven by set_lizard from the manager) handles the
        // desktop-output suppression.

        // Auto-center the sticks: average the first quiet frames. Raw rest
        // offsets were observed up to ~900 on hardware. 30 frames arrive in
        // ~120 ms at the Deck's ~250 Hz rate; keep the window SHORT — this is
        // dead time at every OSK open.
        let mut sums = [0i32; 4];
        let mut n = 0usize;
        let deadline = Instant::now() + Duration::from_millis(150);
        while n < CENTER_FRAMES && Instant::now() < deadline {
            let mut buf = [0u8; 64];
            if let ReadOutcome::Data(m) = self.read_frame(&mut buf, 50) {
                if let Some([lx, ly, rx, ry]) = deck_sticks(&buf[..m]) {
                    sums[0] += lx as i32;
                    sums[1] += ly as i32;
                    sums[2] += rx as i32;
                    sums[3] += ry as i32;
                    n += 1;
                }
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
            // Keep the grab engaged while lizard "off" is desired: evdev
            // nodes can re-appear after a USB blip, so re-engage on the old
            // lizard-refresh cadence (engage is idempotent).
            let refresh = if self.lizard_enabled.load(Ordering::SeqCst) {
                LIZARD_REFRESH_ON
            } else {
                LIZARD_REFRESH_OFF
            };
            if last_lizard.elapsed().as_secs_f32() > refresh {
                let mut grabber = self.grabber.lock().unwrap();
                if !self.lizard_enabled.load(Ordering::SeqCst) {
                    grabber.engage();
                }
                last_lizard = Instant::now();
            }

            let mut buf = [0u8; 64];
            let outcome = self.read_frame(&mut buf, 200);
            match outcome {
                ReadOutcome::Gone => break,
                ReadOutcome::Quiet => continue,
                ReadOutcome::Data(m) => {
                    if let Some(sci) = parse_deck(&buf[..m], centers) {
                        cb(self, &sci);
                    }
                }
            }
        }

        // Teardown: release the grab so the Deck's firmware mouse/keyboard
        // flows to the desktop again, then drop the session (release + close;
        // per the Python driver the kernel driver re-binds on process exit).
        self.opened.store(false, Ordering::SeqCst);
        self.lizard_enabled.store(true, Ordering::SeqCst);
        self.grabber.lock().unwrap().release();
        *self.session.lock().unwrap() = None;
    }
}
