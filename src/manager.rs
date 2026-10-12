//! Controller-side OSK logic: chords, cursor navigation, trigger roles, pad
//! pointers and lizard suppression.
//!
//! Rust port of `adusk/controller.py`'s `ControllerManager` + `input_thread`
//! (trimmed to the Linux Steam Controller / Deck surfaces: no SDL-pad merge,
//! no per-controller settings, no click-through/desktop mode — those come
//! with the tray/settings port).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::classic::ClassicController;
use crate::deck::DeckController;
use crate::diag;
use crate::events::EventMapper;
use crate::osk_state::{Dir, InputState, OskState, Pointer};
use crate::sc::{SCI_NULL, SciInput, buttons as btn};
use crate::triton::SteamController;
use crate::uinput::{Keyboard, Mouse, keys as K};
use crate::vkb;

// Stick thresholds / cadences (controller.py class constants).
const STICK_DEADZONE: f32 = 14000.0;
/// Session-start retry budget for the manager's uinput devices — same race
/// as osk_main's INIT_TRIES (uaccess ACLs settle seconds after login).
/// 100 ms polling over the same 30 s budget: with the Valve controller as
/// the ONLY input device, every retry step delays typing, so the old 1 s
/// steps turned a transient blocker into ~40 s of dead input every login.
const INIT_TRIES: u32 = 300;
const INIT_RETRY_SLEEP: Duration = Duration::from_millis(100);
const STICK_HOLD_DELAY: f32 = 0.5;
const STICK_VOL_REPEAT: f32 = 0.021;
const KBD_STICK_HOLD_DELAY: f32 = 0.35;
const KBD_STICK_REPEAT: f32 = 0.15;
const KBD_STICK_DEADZONE: f32 = STICK_DEADZONE * 1.32;
/// Right-stick mouse constants (controller.py).
const MOUSE_SPEED: f32 = 800.0;
const MOUSE_DEADZONE: f32 = 5000.0;
const MOUSE_EXPONENT: f32 = 5.0;
const MOUSE_MIN: f32 = 0.05;
/// L2/R2 analog actuation point (0..32767) for the OSK Shift/Enter roles.
const TRIGGER_THRESHOLD: i32 = 16384;

/// Which physical device is feeding frames — for haptics + lizard control.
#[derive(Clone)]
pub enum Device {
    Triton(Arc<SteamController>),
    Deck(Arc<DeckController>),
    Classic(Arc<ClassicController>),
}

impl Device {
    /// Lizard toggle. USB-writing devices (Triton/classic feature reports)
    /// run on a THROWAWAY THREAD: the write can block on the reader's device
    /// mutex for a full read cycle and the firmware can take its time — done
    /// inline (input thread), it stalled the frame pipeline >1.5 s and
    /// false-fired the GONE handover mid-typing (observed: repeated spurious
    /// re-activations = full manager reset per keystroke burst). The Deck's
    /// toggle is local (evdev grabs) and stays synchronous.
    fn set_lizard(&self, enabled: bool) {
        match self {
            Device::Triton(sc) => {
                let sc = Arc::clone(sc);
                std::thread::spawn(move || sc.set_lizard(enabled));
            }
            Device::Deck(d) => d.set_lizard(enabled),
            Device::Classic(c) => {
                let c = Arc::clone(c);
                std::thread::spawn(move || c.set_lizard(enabled));
            }
        }
    }

    /// Light UI tick (both pads, short burst). ALL devices: the Deck's
    /// SET_REPORT control transfer must never run on the input thread (it
    /// contends the USB session the 250 Hz reader needs — that was the
    /// typing lag). This is the ONLY haptic left: the pad-click pop was
    /// removed entirely at the user's request (no click feedback).
    pub fn haptic_tick(&self) {
        match self {
            Device::Triton(sc) => {
                let (f, g, c, ms) = env_quad("KEEBS_SC_TICK", [550, -6, 5, 18]);
                let sc = Arc::clone(sc);
                std::thread::spawn(move || sc.haptic_click(f, g, c, ms));
            }
            Device::Deck(d) => {
                let d = Arc::clone(d);
                std::thread::spawn(move || d.haptic_click(550, -6, 5));
            }
            // Classic 2015: no-op (0x8F singer report uncalibrated).
            Device::Classic(c) => c.haptic_click(550, -6, 5),
        }
    }
}

/// Radial stick -> mouse velocity (see controller.py `_mouse_vector`): speed
/// is a function of the stick's DISTANCE from center, so diagonals aren't
/// slower than axes.
fn mouse_vector(x: f32, y: f32) -> (f32, f32) {
    let mag = (x * x + y * y).sqrt();
    if mag <= MOUSE_DEADZONE {
        return (0.0, 0.0);
    }
    let m = ((mag - MOUSE_DEADZONE) / (32767.0 - MOUSE_DEADZONE)).min(1.0);
    let unit = MOUSE_MIN + (1.0 - MOUSE_MIN) * m.powf(MOUSE_EXPONENT);
    let scaled = unit / mag;
    (x * scaled, y * scaled)
}

/// Pad raw value -> absolute pixel coord (controller.py adjust_raw_x/y).
fn adjust_raw(raw: f32, center_fraction: f32, span: f32, scalar: f32, abs_max: f32) -> f32 {
    span * (center_fraction + scalar * raw / abs_max)
}

#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Neutral,
    Up,
    Down,
    Left,
    Right,
}

pub struct ControllerManager {
    kb: Keyboard,
    mouse: Mouse,
    evm: EventMapper,

    open_t: Instant,

    // Rising-edge latches.
    l3_was_pressed: bool,
    b_was_pressed: bool,
    a_was_pressed: bool,
    a_repeat_at: Instant,
    view_was_pressed: bool,
    alt_held_for_tab: bool,
    dpad_prev: u32,
    x_was_pressed: bool,
    x_repeat_at: Instant,
    lt_prev: bool,
    rt_prev: bool,
    lclick_repeat_at: Option<Instant>,
    rclick_repeat_at: Option<Instant>,
    stick_zone_prev: Zone,
    stick_repeat_at: Instant,
    kbd_stick_zone_prev: Zone,
    kbd_stick_repeat_at: Instant,
    open_tick_pending: bool,
    lizard_asserted_on: Option<bool>,
    last_lizard_assert: Instant,
    /// Back-paddle hold-modifiers (user mapping): L4=Ctrl, L5=AltGr, R4=Shift.
    ctrl_held: bool,
    altgr_held: bool,
    shift_held: bool,
    /// R5 (right lower paddle): toggle open/close. (Window dragging was an
    /// X11-only feature — Wayland compositors own window placement, so it is
    /// deliberately not implemented.)
    r5_was_pressed: bool,
    /// Raw X edge tracker for the Steam+X chord (opens the OSK from anywhere).
    x_raw_was_pressed: bool,
    mouse_l_active: bool,
    mouse_r_active: bool,
    mouse_acc_x: f32,
    mouse_acc_y: f32,
    mouse_last_t: Option<Instant>,
    sc_input_previous: SciInput,
}

impl ControllerManager {
    pub fn new() -> std::io::Result<Self> {
        let mut evm = EventMapper::new()?;
        // Face buttons whose action is unconditional ride EventMapper;
        // everything conditional/latching is handled in handle_input.
        evm.set_button_action(btn::Y, "KEY_SPACE");
        Ok(Self {
            kb: Keyboard::new()?,
            mouse: Mouse::new()?,
            evm,
            open_t: Instant::now(),
            l3_was_pressed: false,
            b_was_pressed: false,
            a_was_pressed: false,
            a_repeat_at: Instant::now(),
            view_was_pressed: false,
            alt_held_for_tab: false,
            dpad_prev: 0,
            x_was_pressed: false,
            x_repeat_at: Instant::now(),
            lt_prev: false,
            rt_prev: false,
            lclick_repeat_at: None,
            rclick_repeat_at: None,
            stick_zone_prev: Zone::Neutral,
            stick_repeat_at: Instant::now(),
            kbd_stick_zone_prev: Zone::Neutral,
            kbd_stick_repeat_at: Instant::now(),
            open_tick_pending: true,
            lizard_asserted_on: None,
            last_lizard_assert: Instant::now(),
            ctrl_held: false,
            altgr_held: false,
            shift_held: false,
            r5_was_pressed: false,
            x_raw_was_pressed: false,
            mouse_l_active: false,
            mouse_r_active: false,
            mouse_acc_x: 0.0,
            mouse_acc_y: 0.0,
            mouse_last_t: None,
            sc_input_previous: SCI_NULL,
        })
    }

    pub fn release_held(&mut self, state: &OskState) {
        // Make sure no injected modifier/key ever stays stuck on the OS
        // (sticky OSK modifiers + back-paddle holds).
        self.kb.release_code(K::KEY_LEFTSHIFT);
        self.kb.release_code(K::KEY_RIGHTSHIFT);
        self.kb.release_code(K::KEY_ENTER);
        self.kb.release_code(K::KEY_LEFTALT);
        self.kb.release_code(K::KEY_LEFTCTRL);
        self.kb.release_code(K::KEY_RIGHTALT);
        self.kb.release_code(K::KEY_LEFTMETA);
        self.ctrl_held = false;
        self.altgr_held = false;
        self.shift_held = false;
        state.set_ctrl_latched(false);
        state.set_alt_latched(false);
        state.set_altgr_latched(false);
        state.set_win_latched(false);
        if self.mouse_l_active {
            self.mouse.release("right");
            self.mouse_l_active = false;
        }
        if self.mouse_r_active {
            self.mouse.release("left");
            self.mouse_r_active = false;
        }
        state.set_shift_held(false);
    }

    /// Re-seed every edge latch for a switch of the active device (see
    /// `input_thread`).
    pub fn reset_for_device_switch(&mut self, state: &OskState) {
        self.release_held(state);
        self.open_t = Instant::now();
        self.l3_was_pressed = false;
        self.b_was_pressed = false;
        self.a_was_pressed = false;
        self.a_repeat_at = Instant::now();
        self.view_was_pressed = false;
        self.alt_held_for_tab = false;
        self.dpad_prev = 0;
        self.x_was_pressed = false;
        self.x_repeat_at = Instant::now();
        self.lt_prev = false;
        self.rt_prev = false;
        self.lclick_repeat_at = None;
        self.rclick_repeat_at = None;
        self.stick_zone_prev = Zone::Neutral;
        self.stick_repeat_at = Instant::now();
        self.kbd_stick_zone_prev = Zone::Neutral;
        self.kbd_stick_repeat_at = Instant::now();
        self.open_tick_pending = false;
        self.lizard_asserted_on = None;
        self.ctrl_held = false;
        self.altgr_held = false;
        self.shift_held = false;
        self.r5_was_pressed = false;
        self.x_raw_was_pressed = false;
        self.mouse_acc_x = 0.0;
        self.mouse_acc_y = 0.0;
        self.mouse_last_t = None;
        self.sc_input_previous = SCI_NULL;
    }

    fn trigger_pressed(&self, b: &SciInput, bit: u32, analog: i32) -> bool {
        if b.buttons & bit != 0 {
            return true;
        }
        analog >= TRIGGER_THRESHOLD
    }

    pub fn handle_input(&mut self, state: &OskState, dev: &Device, b: &SciInput) {
        self.evm.process(b);

        if self.open_tick_pending {
            self.open_tick_pending = false;
            dev.haptic_tick();
        }

        let now = Instant::now();

        // "Steam" gates the chords (QAM acts like Steam too).
        let steam_now = b.buttons & (btn::STEAM | btn::QAM) != 0;

        // Steam+X opens the OSK from anywhere (see handle_steam_button).
        self.handle_steam_button(state, b, steam_now);

        // Lizard policy (the "cursor goes back when the keyboard closes"
        // behavior): the OSK owns the controller while visible -> firmware
        // kb/mouse OFF; OSK hidden -> desktop mode, pads drive the system
        // mouse (ON); Steam held -> OFF regardless so chords don't fight
        // firmware output. Re-asserted on change or every ~2 s (the
        // firmware's own timer re-enables on its own schedule).
        let desired_lizard_on = !state.is_visible() && !steam_now;
        if self.lizard_asserted_on != Some(desired_lizard_on) {
            println!(
                "osk: lizard -> {}",
                if desired_lizard_on {
                    "on (desktop)"
                } else {
                    "off (osk owns)"
                }
            );
        }
        if self.lizard_asserted_on != Some(desired_lizard_on)
            || now.duration_since(self.last_lizard_assert).as_secs_f32() > 2.0
        {
            dev.set_lizard(desired_lizard_on);
            self.lizard_asserted_on = Some(desired_lizard_on);
            self.last_lizard_assert = now;
        }

        // Right stick -> system mouse cursor (visible OSK only).
        let dt = match self.mouse_last_t {
            Some(t) => {
                let d = now.duration_since(t).as_secs_f32();
                if d <= 0.0 || d > 0.1 { 1.0 / 60.0 } else { d }
            }
            None => 1.0 / 60.0,
        };
        self.mouse_last_t = Some(now);
        if state.is_visible() {
            let speed = MOUSE_SPEED * dt;
            let (vx, vy) = mouse_vector(b.rstick_x as f32, b.rstick_y as f32);
            self.mouse_acc_x += vx * speed;
            self.mouse_acc_y += -vy * speed; // screen Y grows downward
            let (mx, my) = (self.mouse_acc_x as i32, self.mouse_acc_y as i32);
            self.mouse_acc_x -= mx as f32;
            self.mouse_acc_y -= my as f32;
            if mx != 0 || my != 0 {
                self.mouse.move_rel(mx, my);
            }
        }

        // L3 -> Caps Lock; Steam+L3 -> Play/Pause.
        let l3_pressed = b.buttons & btn::L3 != 0;
        if l3_pressed && !self.l3_was_pressed {
            if steam_now {
                self.kb.tap_code(K::KEY_PLAYPAUSE);
            } else {
                self.kb.tap_code(K::KEY_CAPSLOCK);
                // Caps state is read live from the X server modifier mask —
                // no manual tracking needed.
            }
        }
        self.l3_was_pressed = l3_pressed;

        // Publish touch state (renderer hides hover-cursor while pads touched).
        let lpad_touched = b.buttons & btn::LPADTOUCH != 0;
        let rpad_touched = b.buttons & btn::RPADTOUCH != 0;
        state.set_pad_touched(lpad_touched, rpad_touched);

        // R2/L2 role logic (Shift/Enter) is GONE — the user never uses it.
        // Triggers still act as "press the key under the pointer" while a
        // pad is touched (the handle_pad_input click roles below).
        let lt_pressed = self.trigger_pressed(b, btn::LT, b.ltrig as i32);
        let lt_was = self.lt_prev;
        self.lt_prev = lt_pressed;
        let rt_pressed = self.trigger_pressed(b, btn::RT, b.rtrig as i32);
        let rt_was = self.rt_prev;
        self.rt_prev = rt_pressed;

        // Back paddles (user mapping): L4 = Ctrl hold, L5 = AltGr hold
        // (Polish diacritics!), R4 = Shift hold — same hold mechanic as the
        // old L2 shift (press on rising edge, release on falling). R5
        // toggles the keyboard open/closed below.
        let l4 = b.buttons & btn::LGRIP1 != 0;
        let l5 = b.buttons & btn::LGRIP2 != 0;
        let r4 = b.buttons & btn::RGRIP1 != 0;
        let r5 = b.buttons & btn::RGRIP2 != 0;
        if l4 && !self.ctrl_held {
            self.kb.press_code(K::KEY_LEFTCTRL);
            self.ctrl_held = true;
        } else if !l4 && self.ctrl_held {
            self.kb.release_code(K::KEY_LEFTCTRL);
            self.ctrl_held = false;
        }
        if l5 && !self.altgr_held {
            self.kb.press_code(K::KEY_RIGHTALT);
            self.altgr_held = true;
        } else if !l5 && self.altgr_held {
            self.kb.release_code(K::KEY_RIGHTALT);
            self.altgr_held = false;
        }
        if r4 && !self.shift_held {
            self.kb.press_code(K::KEY_LEFTSHIFT);
            self.shift_held = true;
        } else if !r4 && self.shift_held {
            self.kb.release_code(K::KEY_LEFTSHIFT);
            self.shift_held = false;
        }
        if r5 && !self.r5_was_pressed {
            // R5 toggles: close when open, OPEN when closed (user mapping).
            if state.is_visible() {
                state.close();
            } else {
                state.show();
            }
        }
        self.r5_was_pressed = r5;

        // B closes the keyboard, on rising edge.
        let b_pressed = b.buttons & btn::B != 0;
        if b_pressed && !self.b_was_pressed {
            state.close();
        }
        self.b_was_pressed = b_pressed;

        // Mirror Shift state to the renderer (latch + R4 paddle hold).
        state.set_shift_held(state.is_shift_latched() || self.shift_held);

        // DPAD steps the cursor (queued; main loop knows pixel layout).
        let dpad_mask = btn::DPAD_UP | btn::DPAD_DOWN | btn::DPAD_LEFT | btn::DPAD_RIGHT;
        let dpad_now = b.buttons & dpad_mask;
        let dpad_newly = (self.dpad_prev ^ dpad_now) & dpad_now;
        if dpad_newly & btn::DPAD_UP != 0 {
            state.queue_dpad(Dir::Up, false);
        }
        if dpad_newly & btn::DPAD_DOWN != 0 {
            state.queue_dpad(Dir::Down, false);
        }
        if dpad_newly & btn::DPAD_LEFT != 0 {
            state.queue_dpad(Dir::Left, false);
        }
        if dpad_newly & btn::DPAD_RIGHT != 0 {
            state.queue_dpad(Dir::Right, false);
        }
        self.dpad_prev = dpad_now;

        // A -> press the key under the DPAD cursor (rising edge + repeat).
        let a_pressed = b.buttons & btn::A != 0;
        let (a_row, a_col) = state.get_cursor();
        if a_pressed && !self.a_was_pressed {
            state.queue_key_press(a_row, a_col, false);
            self.a_repeat_at = now + Duration::from_secs_f32(vkb::KEY_REPEAT_DELAY);
        } else if a_pressed && now >= self.a_repeat_at {
            state.queue_key_press(a_row, a_col, true);
            self.a_repeat_at = now + Duration::from_secs_f32(vkb::KEY_REPEAT_INTERVAL);
        }
        if a_pressed {
            state.set_mouse_press_cell(Some((a_row, a_col)));
        } else if self.a_was_pressed {
            state.set_mouse_press_cell(None);
        }
        self.a_was_pressed = a_pressed;

        // Visual highlight: paint keys whose bound button is held.
        let mut highlights = std::collections::HashSet::new();
        if state.is_shift_latched() || self.shift_held {
            highlights.insert(K::KEY_LEFTSHIFT);
            highlights.insert(K::KEY_RIGHTSHIFT);
        }
        // Sticky OSK modifiers glow while latched (the per-frame set is the
        // single source of truth — the latch-time set would be overwritten
        // here one frame later). Back-paddle holds light the same keys.
        if state.is_ctrl_latched() || self.ctrl_held {
            highlights.insert(K::KEY_LEFTCTRL);
        }
        if state.is_altgr_latched() || self.altgr_held {
            highlights.insert(K::KEY_RIGHTALT);
        }
        if state.is_win_latched() {
            highlights.insert(K::KEY_LEFTMETA);
        }
        // Caps glows while ACTUALLY latched on (same source that flips the
        // letter glyphs — previously it only lit while L3 was physically
        // held, so letters could be uppercase with no glowing Caps key),
        // plus transient feedback while L3 is held.
        if (l3_pressed && !steam_now) || state.is_caps_on() {
            highlights.insert(K::KEY_CAPSLOCK);
        }
        if b.buttons & btn::X != 0 {
            highlights.insert(K::KEY_BACKSPACE);
        }
        if b.buttons & btn::Y != 0 {
            highlights.insert(K::KEY_SPACE);
        }
        state.set_highlighted(highlights);

        // X -> Backspace with hold-to-repeat, gated off during Steam holds.
        let x_pressed = (b.buttons & btn::X != 0) && !steam_now;
        if x_pressed && !self.x_was_pressed {
            self.kb.tap_code(K::KEY_BACKSPACE);
            self.x_repeat_at = now + Duration::from_secs_f32(vkb::KEY_REPEAT_DELAY);
        } else if x_pressed && now >= self.x_repeat_at {
            self.kb.tap_code(K::KEY_BACKSPACE);
            self.x_repeat_at = now + Duration::from_secs_f32(vkb::KEY_REPEAT_INTERVAL);
        }
        self.x_was_pressed = x_pressed;

        // Steam + left stick -> media transport.
        self.handle_media_stick(state, dev, b, steam_now, now);

        // Left stick (no Steam) -> move the OSK cursor.
        self.handle_kbd_stick(state, b, steam_now, now);

        // Steam + VIEW -> Alt+Tab (hold Alt through the Steam hold).
        let view_now = b.buttons & btn::VIEW != 0;
        if steam_now && view_now && !self.view_was_pressed {
            if !self.alt_held_for_tab {
                self.kb.press_code(K::KEY_LEFTALT);
                self.alt_held_for_tab = true;
            }
            self.kb.tap_code(K::KEY_TAB);
        }
        self.view_was_pressed = view_now;
        if !steam_now && self.alt_held_for_tab {
            self.kb.release_code(K::KEY_LEFTALT);
            self.alt_held_for_tab = false;
        }

        if self.sc_input_previous == SCI_NULL {
            self.sc_input_previous = *b;
            return;
        }

        // Touchpad pointers (absolute coords in OSK-window space), CLAMPED
        // to the window: an over-gain pad must saturate at the edges, never
        // place the pointer outside the keyboard (dead clicks + imprecise
        // typing).
        let (w, h) = state.abs_dims();
        let gain = pad_gain();
        let lx =
            adjust_raw(b.lpad_x as f32 * gain, 0.25, w, 6.0 / 5.0, 0x20000 as f32).clamp(0.0, w);
        let ly =
            adjust_raw(b.lpad_y as f32 * gain, 0.5, h, -(6.0 / 5.0), 0x10000 as f32).clamp(0.0, h);
        let rx =
            adjust_raw(b.rpad_x as f32 * gain, 0.75, w, 6.0 / 5.0, 0x20000 as f32).clamp(0.0, w);
        let ry =
            adjust_raw(b.rpad_y as f32 * gain, 0.5, h, -(6.0 / 5.0), 0x10000 as f32).clamp(0.0, h);
        let left = self.handle_pad_input(
            state,
            dev,
            b,
            (lx, ly),
            btn::LPADTOUCH,
            btn::LPAD,
            lt_pressed,
            lt_was,
            now,
            true,
        );
        let right = self.handle_pad_input(
            state,
            dev,
            b,
            (rx, ry),
            btn::RPADTOUCH,
            btn::RPAD,
            rt_pressed,
            rt_was,
            now,
            false,
        );
        // Light smoothing (pad_smoothing 0.15 lowpass), like vptr.smoothen,
        // applied in fractional space so it survives resizes.
        let smooth = |cur: (f32, f32), prev: (f32, f32)| -> (f32, f32) {
            (
                cur.0 + (prev.0 - cur.0) * 0.15,
                cur.1 + (prev.1 - cur.1) * 0.15,
            )
        };
        let prev_ptrs = state.get_pointers();
        let l_frac = smooth(state.abs_to_frac(left.0.0, left.0.1), prev_ptrs[0].frac);
        let r_frac = smooth(state.abs_to_frac(right.0.0, right.0.1), prev_ptrs[1].frac);
        state.set_pointers(
            Pointer {
                state: left.1,
                frac: l_frac,
            },
            Pointer {
                state: right.1,
                frac: r_frac,
            },
        );
        self.sc_input_previous = *b;
    }

    /// Steam+X opens the OSK from anywhere. X is also a typing key
    /// (Backspace), so the open rides a dedicated RAW edge tracker.
    fn handle_steam_button(&mut self, state: &OskState, b: &SciInput, steam_now: bool) {
        let x_raw = b.buttons & btn::X != 0;

        if steam_now && x_raw && !self.x_raw_was_pressed {
            state.show();
            self.open_tick_pending = true;
        }

        self.x_raw_was_pressed = x_raw;
    }

    fn handle_pad_input(
        &mut self,
        state: &OskState,
        _dev: &Device,
        b: &SciInput,
        coords: (f32, f32),
        touch_mask: u32,
        click_mask: u32,
        trigger_pressed: bool,
        trigger_prev: bool,
        now: Instant,
        is_left: bool,
    ) -> ((f32, f32), InputState) {
        let prev = self.sc_input_previous.buttons;
        // (Pad-release rumble removed — only the PRESS buzzes, user mapping.)
        if b.buttons & touch_mask == 0 {
            return (coords, InputState::Inactive);
        }
        let trigger_held = trigger_pressed; // click role is always allowed for pads here
        let pad_clicked = b.buttons & click_mask != 0;
        let touch_was = prev & touch_mask != 0;
        let trigger_edge = trigger_held && (!touch_was || !trigger_prev);
        let pad_edge = pad_clicked && (prev & click_mask == 0);
        let click_active = trigger_held || pad_clicked;

        // Hold-to-repeat clock, keyed per pad (is_left) so left/right keep
        // independent timers. The main loop only honors repeat hits over
        // repeatable keys (Backspace/arrows).
        let repeat_at = if is_left {
            &mut self.lclick_repeat_at
        } else {
            &mut self.rclick_repeat_at
        };
        if trigger_edge || pad_edge {
            state.queue_click(state.abs_to_frac(coords.0, coords.1), false);
            *repeat_at = Some(now + Duration::from_secs_f32(vkb::KEY_REPEAT_DELAY));
        } else if click_active {
            let due = match *repeat_at {
                Some(t) => now >= t,
                None => true,
            };
            if due {
                state.queue_click(state.abs_to_frac(coords.0, coords.1), true);
                *repeat_at = Some(now + Duration::from_secs_f32(vkb::KEY_REPEAT_INTERVAL));
            }
        } else {
            *repeat_at = None;
        }
        let st = if click_active {
            InputState::Click
        } else {
            InputState::Hover
        };
        (coords, st)
    }

    fn handle_media_stick(
        &mut self,
        _state: &OskState,
        dev: &Device,
        b: &SciInput,
        steam_now: bool,
        now: Instant,
    ) {
        let x = b.lstick_x as f32;
        let y = b.lstick_y as f32; // positive = up

        let mut zone = Zone::Neutral;
        if steam_now && (x.abs() > STICK_DEADZONE || y.abs() > STICK_DEADZONE) {
            zone = if y.abs() >= x.abs() {
                if y > 0.0 { Zone::Up } else { Zone::Down }
            } else if x > 0.0 {
                Zone::Right
            } else {
                Zone::Left
            };
        }

        let key = match zone {
            Zone::Up => Some(K::KEY_VOLUMEUP),
            Zone::Down => Some(K::KEY_VOLUMEDOWN),
            Zone::Left => Some(K::KEY_PREVIOUSSONG),
            Zone::Right => Some(K::KEY_NEXTSONG),
            Zone::Neutral => None,
        };

        let mut fire = false;
        let mut is_edge = false;
        if zone != self.stick_zone_prev {
            fire = zone != Zone::Neutral;
            is_edge = fire;
            self.stick_repeat_at = now + Duration::from_secs_f32(STICK_HOLD_DELAY);
        } else if matches!(zone, Zone::Up | Zone::Down) && now >= self.stick_repeat_at {
            fire = true;
            self.stick_repeat_at = now + Duration::from_secs_f32(STICK_VOL_REPEAT);
        }
        self.stick_zone_prev = zone;

        if let Some(key) = fire.then_some(key).flatten() {
            self.kb.tap_code(key);
            if is_edge && matches!(zone, Zone::Up | Zone::Down) {
                dev.haptic_tick();
            }
        }
    }

    fn handle_kbd_stick(&mut self, state: &OskState, b: &SciInput, steam_now: bool, now: Instant) {
        let x = b.lstick_x as f32;
        let y = b.lstick_y as f32;

        let mut zone = Zone::Neutral;
        if !steam_now && (x.abs() > KBD_STICK_DEADZONE || y.abs() > KBD_STICK_DEADZONE) {
            zone = if y.abs() >= x.abs() {
                if y > 0.0 { Zone::Up } else { Zone::Down }
            } else if x > 0.0 {
                Zone::Right
            } else {
                Zone::Left
            };
        }

        let fire = if zone != self.kbd_stick_zone_prev {
            // Entering a new direction always steps once (the "tap"), then
            // waits KBD_STICK_HOLD_DELAY before the held auto-repeat begins.
            self.kbd_stick_repeat_at = now + Duration::from_secs_f32(KBD_STICK_HOLD_DELAY);
            zone != Zone::Neutral
        } else if zone != Zone::Neutral && now >= self.kbd_stick_repeat_at {
            self.kbd_stick_repeat_at = now + Duration::from_secs_f32(KBD_STICK_REPEAT);
            true
        } else {
            false
        };
        self.kbd_stick_zone_prev = zone;

        if fire {
            let dir = match zone {
                Zone::Up => Dir::Up,
                Zone::Down => Dir::Down,
                Zone::Left => Dir::Left,
                Zone::Right => Dir::Right,
                Zone::Neutral => unreachable!(),
            };
            state.queue_dpad(dir, true);
        }
    }
}

/// Pad→screen pointer gain (`KEEBS_PAD_GAIN`, default 2.0). The Python
/// abs_max constants (0x20000/0x10000) assume pads with a larger raw range
/// than the Deck/SC 2026 report — without extra gain the pads covered less
/// than half the window. Tune live: `KEEBS_PAD_GAIN=3 nix run .#keebs`.
fn pad_gain() -> f32 {
    static G: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *G.get_or_init(|| {
        std::env::var("KEEBS_PAD_GAIN")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .filter(|v| (0.05..=20.0).contains(v))
            .unwrap_or(1.1)
    })
}

/// SC 2026 haptic tuning (`KEEBS_SC_TICK` / `KEEBS_SC_PADCLICK`, format
/// "freq,gain,count,ms") — the Deck's haptics are firmware-native and stay
/// untouched. Gain is a signed int8: nearer +127 = louder.
fn env_quad(name: &str, default: [i64; 4]) -> (u32, i8, u16, u64) {
    let raw = std::env::var(name).unwrap_or_default();
    let mut v = default;
    for (i, part) in raw.split(',').enumerate().take(4) {
        if let Ok(x) = part.trim().parse::<i64>() {
            v[i] = x;
        }
    }
    (
        v[0].clamp(1, 100_000) as u32,
        v[1].clamp(-127, 127) as i8,
        v[2].clamp(1, 0xFFFF) as u16,
        v[3].clamp(0, 10_000) as u64,
    )
}

/// Which controller the manager is currently bound to (multi-device support:
/// support: every attached controller gets a reader thread; the manager
/// follows the one actually in use, with hysteresis against mid-chord theft).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Deck,
    Triton,
    Classic,
}

/// A frame counts as "in use" ONLY by REAL user intent: any button minus
/// the firmware-emulation noise bits. LGRIP_REST/RGRIP_REST are paddle-rest
/// flags; LPADJOY_TOUCH/RPADJOY_TOUCH are the LIZARD joystick-emulation
/// touch flags that flip ON THEIR OWN while a controller sits in desktop
/// mode — one such bit stole the active slot mid-session (log: activation
/// buttons=0x31000000) and, never idling, starved the other controller
/// ("dead Deck pad after close"). Analog axes stay excluded too (rest
/// offsets / garbage slots). Real intent = real buttons + LPADTOUCH/
/// RPADTOUCH pad-touch bits.
fn frame_has_activity(s: &SciInput) -> bool {
    const NOISE: u32 = btn::LGRIP_REST | btn::RGRIP_REST | btn::LPADJOY_TOUCH | btn::RPADJOY_TOUCH;
    s.buttons & !NOISE != 0
}

struct Reg {
    dev: Device,
    /// Last frame arrival from this device (any content).
    last_frame: Instant,
    /// Last frame with actual input activity.
    last_active: Instant,
    /// Previous frame's button mask, for edge logging while active.
    prev_buttons: u32,
    /// One-shot first-frame diagnostic (rest values of buttons/axes).
    dumped: bool,
    /// Rate telemetry: frames since the last rate report.
    frames: u64,
    /// Rate telemetry: activity-frames since the last rate report.
    activity_frames: u64,
    /// Last lizard state actually SENT to this (inactive) device + when.
    lizard_sent: Option<bool>,
    lizard_sent_at: Instant,
}

fn spawn_deck_reader(
    readers: &mut Vec<(Kind, JoinHandle<()>)>,
    tx: &mpsc::Sender<(Kind, SciInput)>,
    stop: &Arc<crate::stop::StopFlag>,
) -> Option<Device> {
    let d = DeckController::new(true, None, Arc::clone(stop)).ok()?;
    let d = Arc::new(d);
    let d_run = Arc::clone(&d);
    let tx2 = tx.clone();
    let sp = Arc::clone(stop);
    readers.push((
        Kind::Deck,
        std::thread::spawn(move || {
            loop {
                d_run.run(&mut |_c, s| {
                    let _ = tx2.send((Kind::Deck, *s));
                });
                if sp.wait(Duration::from_secs(2)) {
                    break;
                }
            }
        }),
    ));
    Some(Device::Deck(d))
}

fn spawn_triton_reader(
    readers: &mut Vec<(Kind, JoinHandle<()>)>,
    tx: &mpsc::Sender<(Kind, SciInput)>,
    stop: &Arc<crate::stop::StopFlag>,
) -> Option<Device> {
    let sc = SteamController::new(true, Arc::clone(stop)).ok()?;
    let sc = Arc::new(sc);
    let sc_run = Arc::clone(&sc);
    let tx2 = tx.clone();
    let sp = Arc::clone(stop);
    readers.push((
        Kind::Triton,
        std::thread::spawn(move || {
            loop {
                sc_run.run(&mut |_c, s| {
                    let _ = tx2.send((Kind::Triton, *s));
                });
                if sp.wait(Duration::from_secs(2)) {
                    break;
                }
            }
        }),
    ));
    Some(Device::Triton(sc))
}

fn spawn_classic_reader(
    readers: &mut Vec<(Kind, JoinHandle<()>)>,
    tx: &mpsc::Sender<(Kind, SciInput)>,
    stop: &Arc<crate::stop::StopFlag>,
) -> Option<Device> {
    let c = ClassicController::new(true, Arc::clone(stop)).ok()?;
    let c = Arc::new(c);
    let c_run = Arc::clone(&c);
    let tx2 = tx.clone();
    let sp = Arc::clone(stop);
    readers.push((
        Kind::Classic,
        std::thread::spawn(move || {
            loop {
                c_run.run(&mut |_c, s| {
                    let _ = tx2.send((Kind::Classic, *s));
                });
                if sp.wait(Duration::from_secs(2)) {
                    break;
                }
            }
        }),
    ));
    Some(Device::Classic(c))
}

fn spawn_reader(
    kind: Kind,
    readers: &mut Vec<(Kind, JoinHandle<()>)>,
    tx: &mpsc::Sender<(Kind, SciInput)>,
    stop: &Arc<crate::stop::StopFlag>,
) -> Option<Device> {
    match kind {
        Kind::Deck => spawn_deck_reader(readers, tx, stop),
        Kind::Triton => spawn_triton_reader(readers, tx, stop),
        Kind::Classic => spawn_classic_reader(readers, tx, stop),
    }
}

/// Bind the manager to `kind`: previous device goes back to desktop posture
/// (lizard ON), edge latches re-seed FROM THE ACTIVATING FRAME, the haptic
/// hook re-points.
fn activate(
    manager: &mut ControllerManager,
    state: &Arc<OskState>,
    registry: &[(Kind, Reg)],
    active: &mut Option<Kind>,
    kind: Kind,
) {
    let Some((_, new)) = registry.iter().find(|(k, _)| *k == kind) else {
        return;
    };
    if let Some(a) = *active
        && let Some((_, old)) = registry.iter().find(|(k, _)| *k == a)
    {
        // Demoted device follows the global posture: lizard ON only when the
        // OSK is hidden (visible -> OFF, same rule the supervision tick
        // enforces for inactive devices — its firmware mouse must not fight
        // the open keyboard).
        old.dev.set_lizard(!state.is_visible());
    }
    manager.reset_for_device_switch(state);
    let dev = new.dev.clone();
    state.set_haptic_tick(Arc::new(move || {
        let dev = dev.clone();
        std::thread::spawn(move || dev.haptic_tick());
    }));
    *active = Some(kind);
    diag::log(&format!("osk: active controller: {kind:?}"));
}

/// Controller thread body. ONE reader thread per attached device (Deck /
/// Triton 2026 / classic 2015) — all controllers stay live simultaneously,
/// like the Python app's InputSourceMerger — but the OSK manager binds to
/// exactly ONE device at a time: the one showing input activity. Switching
/// requires the current device to have been idle for a beat (hysteresis), so
/// grabbing a second controller mid-chord can't steal input. Devices are
/// opened PASSIVE (lizard ON / desktop posture); the manager asserts lizard
/// policy on the active device per frame, exactly like the single-device
/// path did.
pub fn input_thread(state: Arc<OskState>, stop: Arc<crate::stop::StopFlag>) {
    // The manager opens three uinput devices single-shot; a transient EACCES
    // at session start used to kill this thread permanently while the window
    // kept running ("keebs works only after a manual restart"). Retry.
    let mut manager = None;
    for try_n in 1..=INIT_TRIES {
        match ControllerManager::new() {
            Ok(m) => {
                manager = Some(m);
                break;
            }
            Err(e) => {
                if try_n == 1 || try_n % 10 == 0 {
                    diag::log(&format!(
                        "osk: manager init failed ({e}); retry {try_n}/{INIT_TRIES}"
                    ));
                }
                if try_n < INIT_TRIES {
                    let _ = stop.wait(INIT_RETRY_SLEEP);
                }
            }
        }
    }
    let mut manager = match manager {
        Some(m) => m,
        None => {
            // No injection backend at all — quit the app rather than run as
            // an unresponsive shell nobody can talk to.
            state.close_app.store(true, Ordering::SeqCst);
            return;
        }
    };
    diag::log("manager: uinput devices ready (input path live)");

    /// The active device must be idle this long before another can take over.
    const SWITCH_IDLE: Duration = Duration::from_millis(300);
    /// No frames at all for this long => device considered gone. Generous on
    /// purpose: the supervision tick can legally consume a few hundred ms,
    /// and a false GONE resets the whole manager mid-typing (the "lag").
    const GONE: Duration = Duration::from_millis(3000);
    // Presence enumerations are blocking (~tens of ms each) — amortize to
    // every 3rd supervision tick.
    let mut tick_n: u32 = 0;

    let (tx, rx) = mpsc::channel::<(Kind, SciInput)>();
    let mut registry: Vec<(Kind, Reg)> = Vec::new();
    let mut readers: Vec<(Kind, JoinHandle<()>)> = Vec::new();
    let mut active: Option<Kind> = None;
    let mut absence_noted = false;
    let mut next_supervise = Instant::now();
    let mut next_telemetry = Instant::now() + Duration::from_secs(5);

    while !stop.is_set() && !state.should_close_app() {
        // --- input: block for the next frame, then drain the whole backlog.
        // NEVER one-frame-per-iteration: that design ran three full HID
        // enumerations per frame and throttled 250 Hz controllers to ~12
        // fps — quick taps (Steam+X!) fell between iterations and vanished.
        let mut frames: std::collections::VecDeque<(Kind, SciInput)> = Default::default();
        if let Ok(f) = rx.recv_timeout(Duration::from_millis(200)) {
            frames.push_back(f);
            while let Ok(f) = rx.try_recv() {
                frames.push_back(f);
                if frames.len() >= 2000 {
                    frames.pop_front(); // staleness cap: keep the newest
                }
            }
        }

        // --- 1 Hz supervision tick: hot-plug, telemetry, handover. --------
        let now = Instant::now();
        if now >= next_supervise {
            next_supervise = now + Duration::from_secs(1);
            tick_n += 1;
            // (Re)spawn readers for present devices (hot-plug). Enumerating
            // is blocking, so only every 3rd tick.
            if tick_n.is_multiple_of(3) {
                let checks = [
                    (Kind::Deck, crate::deck::deck_present()),
                    (Kind::Triton, crate::triton::triton_present()),
                    (Kind::Classic, crate::classic::classic_present()),
                ];
                for (kind, present) in checks {
                    if !present {
                        continue;
                    }
                    if readers.iter().any(|(k, h)| *k == kind && !h.is_finished()) {
                        continue;
                    }
                    if let Some(dev) = spawn_reader(kind, &mut readers, &tx, &stop) {
                        registry.retain(|(k, _)| *k != kind);
                        registry.push((
                            kind,
                            Reg {
                                dev,
                                last_frame: Instant::now(),
                                last_active: Instant::now() - SWITCH_IDLE - SWITCH_IDLE,
                                prev_buttons: 0,
                                dumped: false,
                                frames: 0,
                                activity_frames: 0,
                                lizard_sent: None,
                                lizard_sent_at: Instant::now(),
                            },
                        ));
                        diag::log(&format!("osk: {kind:?} reader attached"));
                    }
                }
            } // presence tick (every 3rd)

            if registry.is_empty() {
                if !absence_noted {
                    diag::log("osk: no controller present; waiting…");
                    absence_noted = true;
                }
            } else {
                absence_noted = false;
            }

            if now >= next_telemetry {
                next_telemetry = now + Duration::from_secs(5);
                for (k, r) in registry.iter_mut() {
                    diag::log(&format!(
                        "osk: [{k:?}] telemetry: {} frames, {} with buttons",
                        r.frames, r.activity_frames
                    ));
                    r.frames = 0;
                    r.activity_frames = 0;
                }
            }

            // INACTIVE devices' lizard policy: OFF while the OSK is visible
            // (an idle controller's firmware mouse fights the keyboard —
            // "SC moves the mouse while I type on the Deck"), ON when hidden
            // (desktop posture for everyone). The manager's
            // per-frame policy keeps owning the ACTIVE device; this tick owns
            // the rest. Change-gated + 3 s re-assert, ALWAYS on a throwaway
            // thread — an inline feature write stalls the frame pipeline (it
            // waits on the reader's device mutex; the Triton disable even
            // sleeps 50 ms for its double-tap).
            let vis = state.is_visible();
            for (k, r) in registry.iter_mut() {
                if Some(*k) == active {
                    continue;
                }
                let want = !vis;
                if r.lizard_sent != Some(want)
                    || r.lizard_sent_at.elapsed() > Duration::from_secs(3)
                {
                    r.lizard_sent = Some(want);
                    r.lizard_sent_at = now;
                    let dev = r.dev.clone();
                    std::thread::spawn(move || dev.set_lizard(want));
                    let _ = k;
                }
            }

            // Active device dropped off USB mid-use: hand over to any other
            // device that is still streaming.
            if let Some(a) = active
                && let Some((_, r)) = registry.iter().find(|(k, _)| *k == a)
                && r.last_frame.elapsed() > GONE
            {
                match registry
                    .iter()
                    .find(|(k, r)| *k != a && r.last_frame.elapsed() < GONE)
                {
                    Some((k, _)) => activate(&mut manager, &state, &registry, &mut active, *k),
                    None => {
                        active = None;
                        manager.release_held(&state);
                    }
                }
            }
        } // supervision tick

        for (kind, sci) in frames {
            if let Some((_, r)) = registry.iter_mut().find(|(k, _)| *k == kind) {
                r.last_frame = Instant::now();
                r.frames += 1;
                if frame_has_activity(&sci) {
                    r.last_active = Instant::now();
                    r.activity_frames += 1;
                }
                if !r.dumped {
                    r.dumped = true;
                    // Hands-off rest snapshot: reveals stuck bits and uncentered
                    // axes (the Triton parser, unlike the Deck one, does no
                    // auto-centering — large rest values here are the bug).
                    diag::log(&format!(
                        "osk: [{kind:?}] first frame: buttons=0x{:08X} lt={} rt={} \
                     lpad=({},{}) rpad=({},{}) lstick=({},{}) rstick=({},{})",
                        sci.buttons,
                        sci.ltrig,
                        sci.rtrig,
                        sci.lpad_x,
                        sci.lpad_y,
                        sci.rpad_x,
                        sci.rpad_y,
                        sci.lstick_x,
                        sci.lstick_y,
                        sci.rstick_x,
                        sci.rstick_y
                    ));
                    // Seed the edge logger from the first frame so an already-held
                    // thumb (LPADTOUCH etc.) doesn't print as a phantom press.
                    r.prev_buttons = sci.buttons;
                }
            }

            match active {
                None => {
                    // First device to show real input wins. Print the activating
                    // frame — if a garbage/noise stream ever sneaks past the
                    // readers, this line is where it becomes visible.
                    if frame_has_activity(&sci) {
                        diag::log(&format!(
                            "osk: activation [{kind:?}]: buttons=0x{:08X} lt={} rt={} \
                         lstick=({},{}) rstick=({},{})",
                            sci.buttons,
                            sci.ltrig,
                            sci.rtrig,
                            sci.lstick_x,
                            sci.lstick_y,
                            sci.rstick_x,
                            sci.rstick_y
                        ));
                        activate(&mut manager, &state, &registry, &mut active, kind);
                    }
                }
                Some(a) if a == kind => {
                    if let Some((_, r)) = registry.iter_mut().find(|(k, _)| *k == kind) {
                        // Button-edge log (the active device only — human-paced):
                        // proves chord bits (Steam+X) and reveals any mis-mapped
                        // button in the next diag file.
                        let edges = r.prev_buttons ^ sci.buttons;
                        if edges != 0 {
                            let pressed = edges & sci.buttons;
                            let released = edges & r.prev_buttons;
                            let names = |m: u32| {
                                crate::sc::button_names(m)
                                    .iter()
                                    .map(|s| s.to_string())
                                    .collect::<Vec<_>>()
                                    .join("+")
                            };
                            diag::log(&format!(
                                "osk: [{kind:?}] +{} -{}",
                                if pressed != 0 {
                                    names(pressed)
                                } else {
                                    "-".into()
                                },
                                if released != 0 {
                                    names(released)
                                } else {
                                    "-".into()
                                }
                            ));
                        }
                        r.prev_buttons = sci.buttons;
                        let dev = r.dev.clone();
                        manager.handle_input(&state, &dev, &sci);
                    }
                }
                Some(a) => {
                    // Activity on a non-active device: take over only if the
                    // active one has been idle (hysteresis). The frame that
                    // triggers the switch is dropped — the manager re-seeds.
                    let active_idle = registry
                        .iter()
                        .find(|(k, _)| *k == a)
                        .map(|(_, r)| r.last_active.elapsed() > SWITCH_IDLE)
                        .unwrap_or(true);
                    if frame_has_activity(&sci) && active_idle {
                        activate(&mut manager, &state, &registry, &mut active, kind);
                    }
                }
            }
        } // for frames
    }
    manager.release_held(&state);
    state.close();
}
