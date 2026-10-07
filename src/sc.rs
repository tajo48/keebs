//! Wire-protocol types and constants shared by both drivers.
//!
//! Rust port of `linux/steamcontroller/__init__.py` (protocol half) and the
//! parsing half of `linux/steamcontroller/deck.py`. All magic numbers were
//! calibrated against live hardware — keep them byte-exact.

use std::collections::HashSet;

pub const VENDOR_ID: u16 = 0x28DE;
pub const PRODUCT_ID_PROTEUS: u16 = 0x1304; // Triton wireless dongle ("puck")
pub const PRODUCT_ID_WIRED: u16 = 0x1302; // Steam Controller 2026 wired
pub const PRODUCT_ID_DECK: u16 = 0x1205; // USB-C-tethered Steam Deck
pub const PRODUCT_ID_DONGLE: u16 = 0x1102; // Steam Controller 2015 wireless dongle
pub const PRODUCT_ID_WIRED_2015: u16 = 0x1142; // Steam Controller 2015 wired

/// Triton input-state report type (`SCStatus.INPUT`).
pub const SC_STATUS_INPUT: u16 = 0x42;

// ---------------------------------------------------------------------------
// Button bit assignments (Triton layout, from SDL's TritonButtons enum).
// ---------------------------------------------------------------------------

pub mod buttons {
    pub const A: u32 = 0x0000_0001;
    pub const B: u32 = 0x0000_0002;
    pub const X: u32 = 0x0000_0004;
    pub const Y: u32 = 0x0000_0008;
    pub const QAM: u32 = 0x0000_0010;
    pub const R3: u32 = 0x0000_0020;
    pub const VIEW: u32 = 0x0000_0040;
    pub const RGRIP1: u32 = 0x0000_0080; // right back paddle (Triton R4)
    pub const RGRIP2: u32 = 0x0000_0100; // right back paddle (Triton R5)
    pub const RB: u32 = 0x0000_0200;
    pub const DPAD_DOWN: u32 = 0x0000_0400;
    pub const DPAD_RIGHT: u32 = 0x0000_0800;
    pub const DPAD_LEFT: u32 = 0x0000_1000;
    pub const DPAD_UP: u32 = 0x0000_2000;
    pub const START: u32 = 0x0000_4000; // menu
    pub const L3: u32 = 0x0000_8000;
    pub const STEAM: u32 = 0x0001_0000;
    pub const LGRIP1: u32 = 0x0002_0000; // left back paddle (Triton L4)
    pub const LGRIP2: u32 = 0x0004_0000; // left back paddle (Triton L5)
    pub const LB: u32 = 0x0008_0000;
    pub const RPADJOY_TOUCH: u32 = 0x0010_0000;
    pub const RPADTOUCH: u32 = 0x0020_0000;
    pub const RPAD: u32 = 0x0040_0000;
    pub const RT: u32 = 0x0080_0000;
    pub const LPADJOY_TOUCH: u32 = 0x0100_0000;
    pub const LPADTOUCH: u32 = 0x0200_0000;
    pub const LPAD: u32 = 0x0400_0000;
    pub const LT: u32 = 0x0800_0000;
    pub const RGRIP_REST: u32 = 0x1000_0000;
    pub const LGRIP_REST: u32 = 0x2000_0000;
    // Combined aliases (either paddle).
    pub const LGRIP: u32 = LGRIP1 | LGRIP2;
    pub const RGRIP: u32 = RGRIP1 | RGRIP2;
}

/// Single-bit (mask, name) table for logging / mapping.
pub const BUTTON_NAMES: [(u32, &str); 30] = [
    (buttons::A, "A"),
    (buttons::B, "B"),
    (buttons::X, "X"),
    (buttons::Y, "Y"),
    (buttons::QAM, "QAM"),
    (buttons::R3, "R3"),
    (buttons::VIEW, "VIEW"),
    (buttons::RGRIP1, "RGRIP1"),
    (buttons::RGRIP2, "RGRIP2"),
    (buttons::RB, "RB"),
    (buttons::DPAD_DOWN, "DPAD_DOWN"),
    (buttons::DPAD_RIGHT, "DPAD_RIGHT"),
    (buttons::DPAD_LEFT, "DPAD_LEFT"),
    (buttons::DPAD_UP, "DPAD_UP"),
    (buttons::START, "START"),
    (buttons::L3, "L3"),
    (buttons::STEAM, "STEAM"),
    (buttons::LGRIP1, "LGRIP1"),
    (buttons::LGRIP2, "LGRIP2"),
    (buttons::LB, "LB"),
    (buttons::RPADJOY_TOUCH, "RPADJOY_TOUCH"),
    (buttons::RPADTOUCH, "RPADTOUCH"),
    (buttons::RPAD, "RPAD"),
    (buttons::RT, "RT"),
    (buttons::LPADJOY_TOUCH, "LPADJOY_TOUCH"),
    (buttons::LPADTOUCH, "LPADTOUCH"),
    (buttons::LPAD, "LPAD"),
    (buttons::LT, "LT"),
    (buttons::RGRIP_REST, "RGRIP_REST"),
    (buttons::LGRIP_REST, "LGRIP_REST"),
];

pub fn button_names(mask: u32) -> Vec<&'static str> {
    BUTTON_NAMES
        .iter()
        .filter(|(m, _)| mask & m != 0)
        .map(|(_, n)| *n)
        .collect()
}

// ---------------------------------------------------------------------------
// Input state (the `SteamControllerInput` namedtuple).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SciInput {
    pub status: u16,
    pub seq: u8,
    pub buttons: u32,
    pub ltrig: i16,
    pub rtrig: i16,
    pub lpad_x: i16,
    pub lpad_y: i16,
    pub rpad_x: i16,
    pub rpad_y: i16,
    pub lstick_x: i16,
    pub lstick_y: i16,
    pub rstick_x: i16,
    pub rstick_y: i16,
}

pub const SCI_NULL: SciInput = SciInput {
    status: 0,
    seq: 0,
    buttons: 0,
    ltrig: 0,
    rtrig: 0,
    lpad_x: 0,
    lpad_y: 0,
    rpad_x: 0,
    rpad_y: 0,
    lstick_x: 0,
    lstick_y: 0,
    rstick_x: 0,
    rstick_y: 0,
};

// ---------------------------------------------------------------------------
// Triton report constants.
// ---------------------------------------------------------------------------

/// A firmware bump moved REPORT_STATE 0x42 -> 0x45; layout is identical, so
/// accept either (mirrors SDL's IsStateReportId).
pub const TRITON_INPUT_REPORT_IDS: [u8; 2] = [0x45, 0x42];
pub const TRITON_INPUT_MIN_LEN: usize = 30;

pub const TRITON_BATTERY_REPORT_ID: u8 = 0x43;
/// report[1] charge-state values.
pub const CHARGE_STATE_RESET: u8 = 0;
pub const CHARGE_STATE_DISCHARGING: u8 = 1;
pub const CHARGE_STATE_CHARGING: u8 = 2;
pub const CHARGE_STATE_SOURCE_VALIDATE: u8 = 3;
pub const CHARGE_STATE_CHARGING_DONE: u8 = 4;
/// No input/battery frame for this long => treat the battery as unknown.
pub const BATTERY_FRESH_SECONDS: f32 = 4.0;

/// Wireless link-status report ids (skipped so they aren't mis-parsed).
pub const TRITON_WIRELESS_STATUS_IDS: [u8; 2] = [0x46, 0x79];

// ---------------------------------------------------------------------------
// Feature / output report constants.
// ---------------------------------------------------------------------------

pub const FEATURE_REPORT_ID: u8 = 0x01;
pub const FEATURE_REPORT_LEN: usize = 64;
pub const ID_SET_SETTINGS_VALUES: u8 = 0x87;
pub const SETTING_LIZARD_MODE: u8 = 9;
/// bit0 = kb emulation, bit1 = mouse emulation; 3 = both on (Triton default).
pub const LIZARD_MODE_OFF: u16 = 0;
pub const LIZARD_MODE_ON: u16 = 3;
pub const ID_TURN_OFF_CONTROLLER: u8 = 0x9F;

pub const HID_OUTPUT_REPORT_LEN: usize = 65;
pub const ID_OUT_HAPTIC_LFO_TONE: u8 = 0x83;
pub const ID_OUT_HAPTIC_STOP: u8 = 0x82;

pub const HAPTIC_PAD_LEFT: u8 = 0;
pub const HAPTIC_PAD_RIGHT: u8 = 1;
pub const HAPTIC_RUMBLE_LEFT: u8 = 3;
pub const HAPTIC_RUMBLE_RIGHT: u8 = 4;

/// Signed int8 tone gain: nearer +127 = loudest (loud end can damage motors).
pub const HAPTIC_DEFAULT_GAIN: i8 = -2;
pub const HAPTIC_CLICK_GAIN: i8 = -6;
pub const HAPTIC_PAD_CLICK_GAIN: i8 = -5;
pub const HAPTIC_PAD_CLICK_FREQ: u32 = 500;
/// Two-tone mode-change chime (F#4 -> A4, reversed when switching off).
pub const HAPTIC_CHIME_GAIN: i8 = 3;
pub const CHIME_NOTES: [u32; 2] = [370, 440];
pub const CHIME_DURATIONS: [f32; 2] = [0.10, 0.15];
pub const CHIME_TONE_SECONDS: f32 = 1.0;
pub const CHIME_DETUNE_HZ: u32 = 2;
pub const CHIME_BODY_FREQ: u32 = 147; // D3 pedal
pub const CHIME_BODY_GAIN: i8 = -12;
pub const CHIME_BODY_ACTUATOR: u8 = HAPTIC_RUMBLE_LEFT;

pub const RUMBLE_FREQ_LOW: u32 = 90; // large motor — heavy
pub const RUMBLE_FREQ_HIGH: u32 = 180; // small motor — buzzy
pub const RUMBLE_GAIN_MIN: i32 = -40;
pub const RUMBLE_GAIN_MAX: i32 = -4;
pub const RUMBLE_TONE_SECONDS: f32 = 1.5;
pub const RUMBLE_REFRESH_SECONDS: f32 = 1.0;

// ---------------------------------------------------------------------------
// Report builders.
// ---------------------------------------------------------------------------

/// 65-byte feature report that sets LIZARD_MODE (`mode` 0 = off, 3 = on).
pub const fn build_lizard_report(mode: u16) -> [u8; 65] {
    let mut b = [0u8; FEATURE_REPORT_LEN + 1];
    b[0] = FEATURE_REPORT_ID;
    b[1] = ID_SET_SETTINGS_VALUES;
    b[2] = 3; // 1 ControllerSetting = 1 + 2 bytes
    b[3] = SETTING_LIZARD_MODE;
    b[4] = (mode & 0xFF) as u8;
    b[5] = (mode >> 8) as u8;
    b
}

pub const DISABLE_LIZARD_REPORT: [u8; 65] = build_lizard_report(LIZARD_MODE_OFF);
pub const ENABLE_LIZARD_REPORT: [u8; 65] = build_lizard_report(LIZARD_MODE_ON);

/// Feature report asking the controller to power off (payload "off!").
pub const TURN_OFF_REPORT: [u8; 65] = {
    let mut b = [0u8; FEATURE_REPORT_LEN + 1];
    b[0] = FEATURE_REPORT_ID;
    b[1] = ID_TURN_OFF_CONTROLLER;
    b[2] = 0x04;
    b[3] = b'o';
    b[4] = b'f';
    b[5] = b'f';
    b[6] = b'!';
    b
};

/// 0x83 LFO-tone OUTPUT report: play `freq_hz` on `actuator`. `count` is the
/// burst length; 0x7FFF ~= continuous, small values give crisp clicks.
pub fn build_haptic_tone_report(actuator: u8, freq_hz: u32, gain: i8, count: u16) -> [u8; 65] {
    let mut b = [0u8; HID_OUTPUT_REPORT_LEN];
    b[0] = ID_OUT_HAPTIC_LFO_TONE;
    b[1] = actuator;
    b[2] = gain as u8;
    b[3] = freq_hz as u8;
    b[4] = (freq_hz >> 8) as u8;
    b[5] = count as u8;
    b[6] = (count >> 8) as u8;
    b
}

/// 0x82 stop OUTPUT report for `actuator`.
pub fn build_haptic_stop_report(actuator: u8) -> [u8; 65] {
    let mut b = [0u8; HID_OUTPUT_REPORT_LEN];
    b[0] = ID_OUT_HAPTIC_STOP;
    b[1] = actuator;
    b
}

/// Burst length (cycles) for a tone lasting ~`seconds`, clamped to 16 bits so
/// sustained tones self-expire if the host dies mid-tone.
pub fn tone_count(freq_hz: u32, seconds: f32) -> u16 {
    let cycles = (freq_hz as f32 * seconds) as i64;
    cycles.clamp(1, 0xFFFF) as u16
}

/// XInput motor intensity (1..255) -> signed tone gain in the safe range.
pub fn rumble_gain(intensity: u32) -> i8 {
    let i = intensity.clamp(1, 255) as f32;
    let min = RUMBLE_GAIN_MIN as f32;
    let max = RUMBLE_GAIN_MAX as f32;
    (min + (i / 255.0) * (max - min)).round() as i8
}

// ---------------------------------------------------------------------------
// Triton parsing.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Battery {
    /// 0..100
    pub percent: u8,
    pub charge_state: u8,
    /// Any charge state except Discharging/Reset.
    pub charging: bool,
    pub charge_complete: bool,
    pub voltage_mv: u16,
}

/// Parse a 0x43 power-status report, or `None` if the frame isn't a usable
/// battery reading (firmware placeholders and the 0%-while-not-charging
/// power-off emission are both rejected, as in the Python driver).
pub fn parse_battery(data: &[u8]) -> Option<Battery> {
    if data.len() < 5 || data[0] != TRITON_BATTERY_REPORT_ID {
        return None;
    }
    let percent = data[2];
    if percent > 100 {
        return None;
    }
    let cs = data[1];
    let charging = matches!(
        cs,
        CHARGE_STATE_CHARGING | CHARGE_STATE_SOURCE_VALIDATE | CHARGE_STATE_CHARGING_DONE
    );
    if percent == 0 && !charging {
        return None;
    }
    Some(Battery {
        percent,
        charge_state: cs,
        charging,
        charge_complete: cs == CHARGE_STATE_CHARGING_DONE,
        voltage_mv: u16::from_le_bytes([data[3], data[4]]),
    })
}

/// Parse a Triton input report (0x45 / 0x42) into a [`SciInput`].
///
/// Layout after the report-id byte: seq u8 | buttons u32le | lt i16 | rt i16 |
/// lstick xy | rstick xy | lpad xy | lpad-pressure u16 | rpad xy |
/// rpad-pressure u16.
pub fn parse_triton(data: &[u8]) -> Option<SciInput> {
    if data.len() < TRITON_INPUT_MIN_LEN || !TRITON_INPUT_REPORT_IDS.contains(&data[0]) {
        return None;
    }
    let g = |o: usize| i16::from_le_bytes([data[o], data[o + 1]]);
    Some(SciInput {
        status: SC_STATUS_INPUT,
        seq: data[1],
        buttons: u32::from_le_bytes([data[2], data[3], data[4], data[5]]),
        ltrig: g(6),
        rtrig: g(8),
        lstick_x: g(10),
        lstick_y: g(12),
        rstick_x: g(14),
        rstick_y: g(16),
        lpad_x: g(18),
        lpad_y: g(20),
        rpad_x: g(24),
        rpad_y: g(26),
        ..SCI_NULL
    })
}

// ---------------------------------------------------------------------------
// Deck (0x1205) parsing.
// ---------------------------------------------------------------------------

pub const DECK_INPUT_REPORT_ID: u8 = 0x01;
pub const DECK_INPUT_REPORT_LEN: usize = 64;
/// Flip to -1 if the OSK cursor ever moves the wrong vertical way.
pub const DECK_Y_SIGN: i16 = 1;

/// (byte, bit, button mask), calibrated over calibrate_deck 1/2/5/6.
/// Byte 13 mixes REAL paddle bits (1=L4, 2=R4) with counter bits — only the
/// mapped bits are read.
const DECK_BUTTON_BYTES: [(usize, [u32; 8]); 2] = [
    // byte 8: RT LT RB LB Y B X A
    (
        8,
        [
            buttons::RT,
            buttons::LT,
            buttons::RB,
            buttons::LB,
            buttons::Y,
            buttons::B,
            buttons::X,
            buttons::A,
        ],
    ),
    // byte 9: DUP DRIGHT DLEFT DDOWN VIEW STEAM MENU, bit7 = L5
    (
        9,
        [
            buttons::DPAD_UP,
            buttons::DPAD_RIGHT,
            buttons::DPAD_LEFT,
            buttons::DPAD_DOWN,
            buttons::VIEW,
            buttons::STEAM,
            buttons::START,
            buttons::LGRIP2,
        ],
    ),
];

const fn expand_deck_buttons() -> [(usize, usize, u32); 26] {
    let mut out = [(0usize, 0usize, 0u32); 26];
    let mut i = 0;
    let mut k = 0;
    while k < 2 {
        let (byte, masks) = DECK_BUTTON_BYTES[k];
        let mut bit = 0;
        while bit < 8 {
            out[i] = (byte, bit, masks[bit]);
            i += 1;
            bit += 1;
        }
        k += 1;
    }
    out[16] = (10, 0, buttons::RGRIP2); // R5 paddle
    out[17] = (10, 1, buttons::LPAD); // pad clicks + touch
    out[18] = (10, 2, buttons::RPAD);
    out[19] = (10, 3, buttons::LPADTOUCH);
    out[20] = (10, 4, buttons::RPADTOUCH);
    out[21] = (11, 2, buttons::R3);
    out[22] = (14, 2, buttons::QAM);
    out[23] = (10, 6, buttons::L3); // left stick click
    out[24] = (13, 1, buttons::LGRIP1); // L4 paddle
    out[25] = (13, 2, buttons::RGRIP1); // R4 paddle
    out
}

pub const DECK_BUTTONS: [(usize, usize, u32); 26] = expand_deck_buttons();

/// Raw stick axes (lx, ly, rx, ry) for auto-centering.
pub fn deck_sticks(data: &[u8]) -> Option<[i16; 4]> {
    if data.len() < DECK_INPUT_REPORT_LEN || data[0] != DECK_INPUT_REPORT_ID {
        return None;
    }
    let g = |o: usize| i16::from_le_bytes([data[o], data[o + 1]]);
    Some([g(48), g(50), g(52), g(54)])
}

/// Parse one 64-byte 0x01 Deck frame. `centers` holds the averaged rest
/// offsets for (lx, ly, rx, ry) — raw rest offsets were observed up to ~900.
pub fn parse_deck(data: &[u8], centers: [i16; 4]) -> Option<SciInput> {
    if data.len() < DECK_INPUT_REPORT_LEN || data[0] != DECK_INPUT_REPORT_ID {
        return None;
    }
    let mut b = 0u32;
    for &(byte, bit, mask) in &DECK_BUTTONS {
        if data[byte] & (1 << bit) != 0 {
            b |= mask;
        }
    }
    let g = |o: usize| i16::from_le_bytes([data[o], data[o + 1]]);
    let u = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]);
    let [cx, cy, crx, cry] = centers;
    let y = DECK_Y_SIGN;
    Some(SciInput {
        status: SC_STATUS_INPUT,
        seq: 0,
        buttons: b,
        ltrig: u(44).min(32767) as i16,
        rtrig: u(46).min(32767) as i16,
        lpad_x: g(16),
        lpad_y: g(18) * y,
        rpad_x: g(20),
        rpad_y: g(22) * y,
        lstick_x: g(48) - cx,
        lstick_y: (g(50) - cy) * y,
        rstick_x: g(52) - crx,
        rstick_y: (g(54) - cry) * y,
    })
}

// ---------------------------------------------------------------------------
// Classic Steam Controller (2015) parsing.
// ---------------------------------------------------------------------------
//
// PROVISIONAL LAYOUT — assembled from the ynsta/steamcontroller library and
// SDL's classic `SteamControllerStateReport_t`, NOT yet calibrated against
// live hardware (unlike the Deck/Triton tables above). Run `keebs --probe-sc`
// and fix the offsets below if the decode disagrees:
//
//   byte 0      status (0x01 = input frame)
//   byte 1      seq
//   bytes 4-7   buttons u32le (classic masks, see `classic_buttons`)
//   byte 8      LT analog u8 (0..255)
//   byte 9      RT analog u8 (0..255)
//   bytes 12-15 LPAD x/y i16le
//   bytes 16-19 RPAD x/y i16le
//   (no sticks — the 2015 controller has none; stick fields stay 0)

pub const CLASSIC_INPUT_STATUS: u8 = 0x01;
pub const CLASSIC_INPUT_LEN: usize = 64;

/// Raw classic button masks inside the u32 at bytes 4-7 (provisional — same
/// caveat as the layout above). These are the values the ynsta library used;
/// `RPADTOUCH` is the known compound quirk (0x1800_0000 shares its low bit
/// with `LPADTOUCH`), handled in `parse_classic` instead of the table.
pub mod classic_buttons {
    pub const A: u32 = 0x0000_0002;
    pub const B: u32 = 0x0000_0004;
    pub const X: u32 = 0x0000_0008;
    pub const Y: u32 = 0x0000_0010;
    pub const LB: u32 = 0x0000_0020;
    pub const RB: u32 = 0x0000_0040;
    pub const LT: u32 = 0x0000_0080;
    pub const RT: u32 = 0x0000_0100;
    pub const BACK: u32 = 0x0000_1000; // "SELECT" — maps onto VIEW
    pub const START: u32 = 0x0000_2000;
    pub const LGRIP: u32 = 0x0000_4000;
    pub const RGRIP: u32 = 0x0000_8000;
    pub const STEAM: u32 = 0x0010_0000;
    pub const LPADTOUCH: u32 = 0x0800_0000;
    pub const RPADTOUCH_FULL: u32 = 0x1800_0000;
    pub const LPAD: u32 = 0x1000_0000;
    pub const RPAD: u32 = 0x2000_0000;
}

/// (classic raw mask -> our `buttons::*` mask). Single bits only; compound
/// masks (RPADTOUCH) are special-cased in `parse_classic`.
const CLASSIC_BUTTON_MAP: [(u32, u32); 15] = [
    (classic_buttons::A, buttons::A),
    (classic_buttons::B, buttons::B),
    (classic_buttons::X, buttons::X),
    (classic_buttons::Y, buttons::Y),
    (classic_buttons::LB, buttons::LB),
    (classic_buttons::RB, buttons::RB),
    (classic_buttons::LT, buttons::LT),
    (classic_buttons::RT, buttons::RT),
    (classic_buttons::BACK, buttons::VIEW),
    (classic_buttons::START, buttons::START),
    (classic_buttons::LGRIP, buttons::LGRIP1),
    (classic_buttons::RGRIP, buttons::RGRIP1),
    (classic_buttons::STEAM, buttons::STEAM),
    (classic_buttons::LPADTOUCH, buttons::LPADTOUCH),
    (classic_buttons::LPAD, buttons::LPAD),
];

/// Classic lizard feature reports. Unlike Triton's `0x01`-framed settings
/// reports, the classic protocol puts the command in the report ID itself:
/// `87 03 08 <mode>` (0x07 = kb+mouse off, 0x03 = both on). These magic
/// bytes are the ones the 2015-era drivers (Steam client, sc-controller,
/// ynsta) used. Runtime writes are the documented norm on this hardware —
/// the "persistence trap" note in `triton.rs` is Triton-specific.
pub const CLASSIC_DISABLE_LIZARD_REPORT: [u8; 65] = {
    let mut b = [0u8; 65];
    b[0] = 0x87;
    b[1] = 0x03;
    b[2] = 0x08;
    b[3] = 0x07;
    b
};
pub const CLASSIC_ENABLE_LIZARD_REPORT: [u8; 65] = {
    let mut b = [0u8; 65];
    b[0] = 0x87;
    b[1] = 0x03;
    b[2] = 0x08;
    b[3] = 0x03;
    b
};

/// Parse one classic 64-byte 0x01 frame into a [`SciInput`]. Sticks stay 0
/// (the 2015 controller has none — pads drive the OSK). Analog u8 triggers
/// are scaled to the 0..32767 range the manager's `TRIGGER_THRESHOLD`
/// expects (full pull = 0xFF<<7 = 32640).
pub fn parse_classic(data: &[u8]) -> Option<SciInput> {
    if data.len() < CLASSIC_INPUT_LEN || data[0] != CLASSIC_INPUT_STATUS {
        return None;
    }
    let raw = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let mut b = 0u32;
    for &(classic, ours) in &CLASSIC_BUTTON_MAP {
        if raw & classic != 0 {
            b |= ours;
        }
    }
    // Compound quirk: classic RPADTOUCH is 0x1800_0000 (its low bit is shared
    // with LPADTOUCH), so match the full value, not any-bit.
    if raw & classic_buttons::RPADTOUCH_FULL == classic_buttons::RPADTOUCH_FULL {
        b |= buttons::RPADTOUCH;
    }
    if raw & classic_buttons::RPAD != 0 {
        b |= buttons::RPAD;
    }
    let g = |o: usize| i16::from_le_bytes([data[o], data[o + 1]]);
    Some(SciInput {
        status: SC_STATUS_INPUT,
        seq: data[1],
        buttons: b,
        ltrig: (data[8] as i16) << 7,
        rtrig: (data[9] as i16) << 7,
        lpad_x: g(12),
        lpad_y: g(14),
        rpad_x: g(16),
        rpad_y: g(18),
        ..SCI_NULL
    })
}

/// Cheap presence probe: every Valve product id currently enumerable on USB.
pub fn present_product_ids(api: &hidapi::HidApi) -> HashSet<u16> {
    api.device_list()
        .filter(|d| d.vendor_id() == VENDOR_ID)
        .map(|d| d.product_id())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the PROVISIONAL classic layout: if `--probe-sc` forces a table
    /// fix, update this test along with the offsets above.
    #[test]
    fn parse_classic_maps_layout() {
        let mut f = [0u8; 64];
        f[0] = CLASSIC_INPUT_STATUS;
        f[1] = 42; // seq
        let raw = classic_buttons::A
            | classic_buttons::B
            | classic_buttons::X
            | classic_buttons::Y
            | classic_buttons::LB
            | classic_buttons::RB
            | classic_buttons::BACK
            | classic_buttons::START
            | classic_buttons::LGRIP
            | classic_buttons::RGRIP
            | classic_buttons::STEAM
            | classic_buttons::LPADTOUCH
            | classic_buttons::LPAD
            | classic_buttons::RPADTOUCH_FULL
            | classic_buttons::RPAD;
        f[4..8].copy_from_slice(&raw.to_le_bytes());
        f[8] = 0xFF; // LT fully pulled
        f[9] = 0x40; // RT half pulled
        f[12..14].copy_from_slice(&(-1000i16).to_le_bytes()); // lpad_x
        f[14..16].copy_from_slice(&2000i16.to_le_bytes()); // lpad_y
        f[16..18].copy_from_slice(&3000i16.to_le_bytes()); // rpad_x
        f[18..20].copy_from_slice(&(-4000i16).to_le_bytes()); // rpad_y

        let sci = parse_classic(&f).expect("valid frame");
        assert_eq!(sci.status, SC_STATUS_INPUT);
        assert_eq!(sci.seq, 42);
        let expect_buttons = buttons::A
            | buttons::B
            | buttons::X
            | buttons::Y
            | buttons::LB
            | buttons::RB
            | buttons::VIEW
            | buttons::START
            | buttons::LGRIP1
            | buttons::RGRIP1
            | buttons::STEAM
            | buttons::LPADTOUCH
            | buttons::LPAD
            | buttons::RPADTOUCH
            | buttons::RPAD;
        assert_eq!(sci.buttons, expect_buttons);
        assert_eq!(sci.ltrig, 0xFF << 7);
        assert_eq!(sci.rtrig, 0x40 << 7);
        assert_eq!(sci.lpad_x, -1000);
        assert_eq!(sci.lpad_y, 2000);
        assert_eq!(sci.rpad_x, 3000);
        assert_eq!(sci.rpad_y, -4000);
        // No sticks on the 2015 hardware.
        assert_eq!(
            (sci.lstick_x, sci.lstick_y, sci.rstick_x, sci.rstick_y),
            (0, 0, 0, 0)
        );
        // LT alone (analog, past the manager's threshold) must read as a
        // digital press too — the OSK Shift role depends on it.
        assert!(sci.ltrig >= 16384);
    }

    #[test]
    fn parse_classic_rejects_bad_frames() {
        assert!(parse_classic(&[0u8; 64]).is_none()); // wrong status byte
        let mut f = [0u8; 64];
        f[0] = CLASSIC_INPUT_STATUS;
        assert!(parse_classic(&f[..32]).is_none()); // too short
    }
}
