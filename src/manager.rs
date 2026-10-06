//! Controller-side OSK logic: chords, cursor navigation, trigger roles, pad
//! pointers and lizard suppression.
//!
//! Rust port of `adusk/controller.py`'s `ControllerManager` + `input_thread`
//! (trimmed to the Linux Steam Controller / Deck surfaces: no SDL-pad merge,
//! no per-controller settings, no click-through/desktop mode — those come
//! with the tray/settings port).

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::deck::DeckController;
use crate::events::EventMapper;
use crate::osk_state::{Dir, InputState, OskState, Pointer};
use crate::sc::{SCI_NULL, SciInput, buttons as btn};
use crate::triton::SteamController;
use crate::uinput::{Keyboard, Mouse, keys as K};
use crate::vkb;

// Stick thresholds / cadences (controller.py class constants).
const STICK_DEADZONE: f32 = 14000.0;
const STICK_HOLD_DELAY: f32 = 0.5;
const STICK_VOL_REPEAT: f32 = 0.021;
const KBD_STICK_HOLD_DELAY: f32 = 0.35;
const KBD_STICK_REPEAT: f32 = 0.15;
const KBD_STICK_DEADZONE: f32 = STICK_DEADZONE * 1.32;
/// Grace window after the OSK opens during which a Steam release is NOT a
/// close gesture (covers the HID handoff blip).
const OPEN_CLOSE_GRACE: f32 = 1.0;
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
}

impl Device {
    fn set_lizard(&self, enabled: bool) {
        match self {
            Device::Triton(sc) => sc.set_lizard(enabled),
            Device::Deck(d) => d.set_lizard(enabled),
        }
    }

    /// Light UI tick (both pads, short burst).
    pub fn haptic_tick(&self) {
        match self {
            Device::Triton(sc) => {
                sc.haptic_click(550, -6, 5, 18);
            }
            // Deck: fire BOTH the classic 0x8F dual-vibration report AND the
            // Python driver's 0x83 trackpad tones. The firmware ACKs the 0x8F
            // without buzzing (accepted ≠ felt), so the 0x83 channels — the
            // only haptics the Python deck driver ever sent — must not be
            // short-circuited by it. Duplicates are benign (same tick twice
            // at worst).
            Device::Deck(d) => {
                d.rumble(0xFFFF, 0xFFFF);
                d.haptic_click(550, -6, 5);
            }
        }
    }

    /// Stronger pad-click tick.
    pub fn pad_click_haptic(&self) {
        match self {
            Device::Triton(sc) => {
                sc.haptic_pad_click();
            }
            Device::Deck(d) => {
                d.rumble(0xFFFF, 0xFFFF);
                d.haptic_pad_click();
            }
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

    // Steam+X / Steam-alone chord tracking. Seeded TRUE: the OSK was just
    // opened by a Steam(+X) chord that may still be held on the first frame.
    steam_was_pressed: bool,
    saw_x_during_steam: bool,
    open_t: Instant,

    // Rising-edge latches.
    l3_was_pressed: bool,
    b_was_pressed: bool,
    lgrip_was_pressed: bool,
    a_was_pressed: bool,
    a_repeat_at: Instant,
    view_was_pressed: bool,
    alt_held_for_tab: bool,
    dpad_prev: u32,
    x_was_pressed: bool,
    x_repeat_at: Instant,
    lt_prev: bool,
    rt_prev: bool,
    lt_role: Option<Role>,
    lclick_repeat_at: Option<Instant>,
    rclick_repeat_at: Option<Instant>,
    stick_zone_prev: Zone,
    stick_repeat_at: Instant,
    kbd_stick_zone_prev: Zone,
    kbd_stick_repeat_at: Instant,
    open_tick_pending: bool,
    lizard_asserted_on: Option<bool>,
    last_lizard_assert: Instant,
    shift_active: bool,
    enter_active: bool,
    mouse_l_active: bool,
    mouse_r_active: bool,
    mouse_acc_x: f32,
    mouse_acc_y: f32,
    mouse_last_t: Option<Instant>,
    sc_input_previous: SciInput,
}

// `Mouse` arm arrives with the desktop-mode/click-through port.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq)]
enum Role {
    Click,
    Mouse,
    Shift,
}

impl ControllerManager {
    pub fn new() -> std::io::Result<Self> {
        let mut evm = EventMapper::new()?;
        // Face buttons whose action is unconditional ride EventMapper;
        // everything conditional/latching is handled in handle_input.
        evm.set_button_action(btn::Y, "KEY_SPACE");
        evm.set_button_action(btn::RGRIP1, "KEY_SPACE"); // R4/R5 -> Space (Steam OSK mapping)
        evm.set_button_action(btn::RGRIP2, "KEY_SPACE");
        Ok(Self {
            kb: Keyboard::new()?,
            mouse: Mouse::new()?,
            evm,
            steam_was_pressed: true,
            saw_x_during_steam: true,
            open_t: Instant::now(),
            l3_was_pressed: false,
            b_was_pressed: false,
            lgrip_was_pressed: false,
            a_was_pressed: false,
            a_repeat_at: Instant::now(),
            view_was_pressed: false,
            alt_held_for_tab: false,
            dpad_prev: 0,
            x_was_pressed: false,
            x_repeat_at: Instant::now(),
            lt_prev: false,
            rt_prev: false,
            lt_role: None,
            lclick_repeat_at: None,
            rclick_repeat_at: None,
            stick_zone_prev: Zone::Neutral,
            stick_repeat_at: Instant::now(),
            kbd_stick_zone_prev: Zone::Neutral,
            kbd_stick_repeat_at: Instant::now(),
            open_tick_pending: true,
            lizard_asserted_on: None,
            last_lizard_assert: Instant::now(),
            shift_active: false,
            enter_active: false,
            mouse_l_active: false,
            mouse_r_active: false,
            mouse_acc_x: 0.0,
            mouse_acc_y: 0.0,
            mouse_last_t: None,
            sc_input_previous: SCI_NULL,
        })
    }

    pub fn release_held(&mut self, state: &OskState) {
        // Make sure Shift/Enter/mouse buttons never stay stuck on the OS.
        self.kb.release_code(K::KEY_LEFTSHIFT);
        self.kb.release_code(K::KEY_RIGHTSHIFT);
        self.kb.release_code(K::KEY_ENTER);
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

        // "Steam" gates the chords (QAM acts like Steam too).
        let steam_now = b.buttons & (btn::STEAM | btn::QAM) != 0;

        // L3 -> Caps Lock; Steam+L3 -> Play/Pause.
        let l3_pressed = b.buttons & btn::L3 != 0;
        if l3_pressed && !self.l3_was_pressed {
            if steam_now {
                self.kb.tap_code(K::KEY_PLAYPAUSE);
                self.saw_x_during_steam = true;
            } else {
                self.kb.tap_code(K::KEY_CAPSLOCK);
                state.toggle_caps();
            }
        }
        self.l3_was_pressed = l3_pressed;

        // Publish touch state (renderer hides hover-cursor while pads touched).
        let lpad_touched = b.buttons & btn::LPADTOUCH != 0;
        let rpad_touched = b.buttons & btn::RPADTOUCH != 0;
        state.set_pad_touched(lpad_touched, rpad_touched);

        // L2 role, fixed at press: pad touched -> "click"; else -> "shift".
        let lt_pressed = self.trigger_pressed(b, btn::LT, b.ltrig as i32);
        let lt_was = self.lt_prev;
        if lt_pressed && !self.lt_prev {
            self.lt_role = Some(if lpad_touched {
                Role::Click
            } else {
                Role::Shift
            });
        } else if !lt_pressed {
            self.lt_role = None;
        }
        self.lt_prev = lt_pressed;

        let shift_should_hold = lt_pressed && self.lt_role == Some(Role::Shift);
        if shift_should_hold && !self.shift_active {
            self.kb.press_code(K::KEY_LEFTSHIFT);
            self.shift_active = true;
            dev.pad_click_haptic();
        } else if !shift_should_hold && self.shift_active {
            self.kb.release_code(K::KEY_LEFTSHIFT);
            self.shift_active = false;
        }

        // R2: right pad touched -> key press (pad path); else -> Enter hold.
        let rt_pressed = self.trigger_pressed(b, btn::RT, b.rtrig as i32);
        let rt_was = self.rt_prev;
        self.rt_prev = rt_pressed;
        let enter_should_hold = rt_pressed && !rpad_touched;
        if enter_should_hold && !self.enter_active {
            self.kb.press_code(K::KEY_ENTER);
            self.enter_active = true;
            dev.pad_click_haptic();
        } else if !enter_should_hold && self.enter_active {
            self.kb.release_code(K::KEY_ENTER);
            self.enter_active = false;
        }

        // Mirror Shift state to the renderer (OR in the mouse latch).
        state.set_shift_held(self.shift_active || state.is_shift_latched());

        // B and L4/L5 (LGRIP) close the keyboard, on rising edge.
        let b_pressed = b.buttons & btn::B != 0;
        if b_pressed && !self.b_was_pressed {
            state.close();
        }
        self.b_was_pressed = b_pressed;

        let lgrip_pressed = b.buttons & btn::LGRIP != 0;
        if lgrip_pressed && !self.lgrip_was_pressed {
            state.close();
        }
        self.lgrip_was_pressed = lgrip_pressed;

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
        if self.shift_active || state.is_shift_latched() {
            highlights.insert(K::KEY_LEFTSHIFT);
            highlights.insert(K::KEY_RIGHTSHIFT);
        }
        if l3_pressed && !steam_now {
            highlights.insert(K::KEY_CAPSLOCK);
        }
        if b.buttons & btn::X != 0 {
            highlights.insert(K::KEY_BACKSPACE);
        }
        if self.enter_active {
            highlights.insert(K::KEY_ENTER);
        }
        if b.buttons & (btn::Y | btn::RGRIP) != 0 {
            highlights.insert(K::KEY_SPACE);
        }
        state.set_highlighted(highlights);

        // Steam+X opens the keyboard; Steam released alone closes it.
        let x_now = b.buttons & btn::X != 0;
        if steam_now && !self.steam_was_pressed {
            self.saw_x_during_steam = false;
        }
        if steam_now && x_now && !self.saw_x_during_steam {
            self.saw_x_during_steam = true;
            state.show();
        }

        // Lizard policy (the "cursor goes back when the keyboard closes"
        // behavior): the OSK owns the controller while visible -> firmware
        // kb/mouse OFF; OSK hidden -> desktop mode, pads drive the system
        // mouse (ON); Steam held -> OFF regardless so chords don't fight
        // firmware output. Re-asserted on change or every ~2 s (the
        // firmware's own timer re-enables on its own schedule).
        let desired_lizard_on = !state.is_visible() && !steam_now;
        if self.lizard_asserted_on != Some(desired_lizard_on)
            || now.duration_since(self.last_lizard_assert).as_secs_f32() > 2.0
        {
            dev.set_lizard(desired_lizard_on);
            self.lizard_asserted_on = Some(desired_lizard_on);
            self.last_lizard_assert = now;
        }

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
            self.saw_x_during_steam = true;
        }
        self.view_was_pressed = view_now;
        if !steam_now && self.alt_held_for_tab {
            self.kb.release_code(K::KEY_LEFTALT);
            self.alt_held_for_tab = false;
        }

        // Steam released alone -> close (past the grace window).
        if self.steam_was_pressed
            && !steam_now
            && !self.saw_x_during_steam
            && now.duration_since(self.open_t).as_secs_f32() > OPEN_CLOSE_GRACE
        {
            state.close();
        }
        self.steam_was_pressed = steam_now;

        if self.sc_input_previous == SCI_NULL {
            self.sc_input_previous = *b;
            return;
        }

        // Touchpad pointers (absolute coords in OSK-window space).
        let (w, h) = state.abs_dims();
        let lx = adjust_raw(b.lpad_x as f32, 0.25, w, 6.0 / 5.0, 0x20000 as f32);
        let ly = adjust_raw(b.lpad_y as f32, 0.5, h, -(6.0 / 5.0), 0x10000 as f32);
        let rx = adjust_raw(b.rpad_x as f32, 0.75, w, 6.0 / 5.0, 0x20000 as f32);
        let ry = adjust_raw(b.rpad_y as f32, 0.5, h, -(6.0 / 5.0), 0x10000 as f32);
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

    #[allow(clippy::too_many_arguments)]
    fn handle_pad_input(
        &mut self,
        state: &OskState,
        dev: &Device,
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
        // Pad-release rumbles too, so a click feels like a full button.
        if (b.buttons & click_mask == 0) && (prev & click_mask != 0) {
            dev.pad_click_haptic();
        }
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
            dev.pad_click_haptic();
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
            self.saw_x_during_steam = true;
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

/// Controller thread body: whichever device is present feeds `handle_input`
/// at its native rate; reconnects on loss. The OSK owns the controller
/// (lizard OFF at open, restored on teardown) — `passive=false`.
pub fn input_thread(state: Arc<OskState>, stop: Arc<crate::stop::StopFlag>) {
    let mut manager = match ControllerManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("osk: uinput init failed ({e}); controller input disabled");
            return;
        }
    };
    let mut absence_noted = false;

    while !stop.is_set() && !state.should_close_app() {
        let device = if crate::deck::deck_present() {
            println!("osk: Steam Deck connected (28de:1205)");
            DeckController::new(false, None, Arc::clone(&stop))
                .ok()
                .map(|d| Device::Deck(Arc::new(d)))
        } else if crate::triton::triton_present() {
            println!("osk: Steam Controller 2026 connected");
            SteamController::new(false, Arc::clone(&stop))
                .ok()
                .map(|sc| Device::Triton(Arc::new(sc)))
        } else {
            if !absence_noted {
                println!("osk: no controller present; waiting…");
                absence_noted = true;
            }
            None
        };

        if let Some(device) = device {
            absence_noted = false;
            // Publish the haptic hooks so the main thread can buzz on each
            // dispatched key (the Python state.set_haptic_tick equivalent).
            {
                let dev = device.clone();
                state.set_haptic_tick(Arc::new(move || dev.haptic_tick()));
            }
            match &device {
                Device::Deck(d) => {
                    let st = Arc::clone(&state);
                    let dev = Device::Deck(Arc::clone(d));
                    d.run(&mut |_c, sci| {
                        manager.handle_input(&st, &dev, sci);
                    });
                }
                Device::Triton(sc) => {
                    let st = Arc::clone(&state);
                    let dev = Device::Triton(Arc::clone(sc));
                    sc.run(&mut |_c, sci| {
                        manager.handle_input(&st, &dev, sci);
                    });
                }
            }
            manager.release_held(&state);
            // Device dropped/teardown restores lizard via the drivers.
        }

        if stop.wait(Duration::from_secs(2)) {
            break;
        }
    }
    manager.release_held(&state);
    state.close();
}
