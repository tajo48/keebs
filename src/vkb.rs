//! Virtual keyboard: layout (embedded from `linux/data/cfg/keyboard-layout.yaml`),
//! grid geometry, cursor stepping and key dispatch.
//!
//! Rust port of `adusk/vkb.py`. The YAML is embedded as static data so the
//! crate builds standalone; the loaded copy keeps the same width weights,
//! modifier styling and behaviors as the live config.

use crate::osk_state::{Dir, OskState};
use crate::uinput::{Keyboard, keys as K};

// Hold-to-repeat cadence shared by EVERY "press a key" input path —
// 2x faster than the original Python feel (user preference): 200 ms
// initial delay, 10 cps repeat.
pub const KEY_REPEAT_DELAY: f32 = 0.2;
pub const KEY_REPEAT_INTERVAL: f32 = 0.1;

const PADDING_INNER: f32 = 3.0;
const PADDING_OUTER: f32 = 2.0;

impl VirtualKeyboard {
    /// All key layouts, computed row-major (rows are 12-14 keys; fine to
    /// materialize).
    pub fn all_key_layouts(&self) -> Vec<KeyLayout> {
        let mut out = Vec::with_capacity(70);
        let mut iterated_y = PADDING_OUTER;
        for (row, keys) in self.keys.iter().enumerate() {
            let mut iterated_x = PADDING_OUTER;
            for (col, key) in keys.iter().enumerate() {
                out.push(KeyLayout {
                    x: iterated_x + PADDING_INNER,
                    y: iterated_y + PADDING_INNER,
                    w: key.width_weight * self.key_width[row],
                    h: self.key_height,
                    row,
                    col,
                });
                iterated_x += key.width_weight * self.key_width[row] + PADDING_INNER * 2.0;
            }
            iterated_y += self.key_height + PADDING_INNER * 2.0;
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Behavior {
    Generic,
    Shift,
    Paste,
    Emoji,
    Move,
    /// Sticky Ctrl modifier (presses the real KEY_LEFTCTRL until toggled off
    /// or the OSK closes) — same mechanism as Shift.
    Ctrl,
    /// Sticky Alt modifier, same mechanism.
    Alt,
    /// Sticky AltGr (Right Alt) — Polish diacritics (ą, ę, ś…) on the
    /// programmer layout ride AltGr+letter.
    AltGr,
    /// Sticky Win/Super (Left Meta).
    Win,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, PartialEq)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Copy)]
pub struct KeyDef {
    pub label: &'static str,
    pub shifted: Option<&'static str>,
    pub keycode: u16,
    pub behavior: Behavior,
    pub width_weight: f32,
    pub modifier: bool,
    pub align: Align,
    pub valign: VAlign,
    pub font_size: Option<f32>,
    pub glyph: Option<&'static str>,
    /// Alternate keycode sent when Shift is held (◀/▶ -> ▲/▼), WITHOUT the OS
    /// seeing a Shift modifier.
    pub shift_keycode: Option<u16>,
    /// Draw the label with the DejaVu symbol-capable font (arrows etc.).
    pub symbol_font: bool,
    /// Small grey controller-button hint ("X", "R2", …) drawn in a corner —
    /// the textual stand-in for the Python app's button-hint icons.
    pub hint: Option<&'static str>,
}

const fn key(label: &'static str, shifted: Option<&'static str>, keycode: u16) -> KeyDef {
    KeyDef {
        label,
        shifted,
        keycode,
        behavior: Behavior::Generic,
        width_weight: 1.0,
        modifier: false,
        align: Align::Center,
        valign: VAlign::Center,
        font_size: None,
        glyph: None,
        shift_keycode: None,
        symbol_font: false,
        hint: None,
    }
}

const fn mod_key(
    label: &'static str,
    keycode: u16,
    behavior: Behavior,
    weight: f32,
    align: Align,
    valign: VAlign,
) -> KeyDef {
    KeyDef {
        width_weight: weight,
        modifier: true,
        align,
        valign,
        behavior,
        ..key(label, None, keycode)
    }
}

/// The QWERTY layout from `keyboard-layout.yaml` (derived from Steam's own
/// VirtualKeyboard config). Symbol production rides the OS Shift modifier, so
/// keycodes are always the unshifted Linux keycodes.
pub fn default_layout() -> Vec<Vec<KeyDef>> {
    use Align::{Left, Right};
    use VAlign::{Bottom, Top};
    vec![
        vec![
            key("`", Some("~"), 41), // KEY_GRAVE
            key("1", Some("!"), 2),
            key("2", Some("@"), 3),
            key("3", Some("#"), 4),
            key("4", Some("$"), 5),
            key("5", Some("%"), 6),
            key("6", Some("^"), 7),
            key("7", Some("&"), 8),
            key("8", Some("*"), 9),
            key("9", Some("("), 10),
            key("0", Some(")"), 11),
            key("-", Some("_"), 12),
            key("=", Some("+"), 13),
            KeyDef {
                glyph: Some("\u{232B}"), // ⌫
                hint: Some("X"),
                ..mod_key(
                    "Bksp",
                    K::KEY_BACKSPACE,
                    Behavior::Generic,
                    1.4,
                    Left,
                    Bottom,
                )
            },
        ],
        vec![
            mod_key("Tab", K::KEY_TAB, Behavior::Generic, 1.4, Left, Bottom),
            key("q", Some("Q"), 16),
            key("w", Some("W"), 17),
            key("e", Some("E"), 18),
            key("r", Some("R"), 19),
            key("t", Some("T"), 20),
            key("y", Some("Y"), 21),
            key("u", Some("U"), 22),
            key("i", Some("I"), 23),
            key("o", Some("O"), 24),
            key("p", Some("P"), 25),
            key("[", Some("{"), 26),
            key("]", Some("}"), 27),
            key("\\", Some("|"), 43),
        ],
        vec![
            KeyDef {
                glyph: Some("\u{21EA}"), // ⇪
                ..mod_key(
                    "Caps",
                    K::KEY_CAPSLOCK,
                    Behavior::Generic,
                    1.6,
                    Left,
                    Bottom,
                )
            },
            key("a", Some("A"), 30),
            key("s", Some("S"), 31),
            key("d", Some("D"), 32),
            key("f", Some("F"), 33),
            key("g", Some("G"), 34),
            key("h", Some("H"), 35),
            key("j", Some("J"), 36),
            key("k", Some("K"), 37),
            key("l", Some("L"), 38),
            key(";", Some(":"), 39),
            key("'", Some("\""), 40),
            KeyDef {
                glyph: Some("\u{21B5}"), // ↵
                ..mod_key("Enter", K::KEY_ENTER, Behavior::Generic, 1.7, Right, Bottom)
            },
        ],
        vec![
            KeyDef {
                glyph: Some("\u{21E7}"), // ⇧
                hint: Some("R4"),
                ..mod_key(
                    "Shift",
                    K::KEY_LEFTSHIFT,
                    Behavior::Shift,
                    2.0,
                    Left,
                    Bottom,
                )
            },
            key("z", Some("Z"), 44),
            key("x", Some("X"), 45),
            key("c", Some("C"), 46),
            key("v", Some("V"), 47),
            key("b", Some("B"), 48),
            key("n", Some("N"), 49),
            key("m", Some("M"), 50),
            key(",", Some("<"), 51),
            key(".", Some(">"), 52),
            key("/", Some("?"), 53),
            KeyDef {
                glyph: Some("\u{21E7}"), // ⇧
                ..mod_key(
                    "Shift",
                    K::KEY_RIGHTSHIFT,
                    Behavior::Shift,
                    2.0,
                    Right,
                    Bottom,
                )
            },
        ],
        vec![
            KeyDef {
                hint: Some("L4"),
                ..mod_key(
                    "Ctrl",
                    K::KEY_LEFTCTRL,
                    Behavior::Ctrl,
                    1.0,
                    Align::Center,
                    VAlign::Center,
                )
            },
            mod_key(
                "Alt",
                K::KEY_LEFTALT,
                Behavior::Alt,
                1.0,
                Align::Center,
                VAlign::Center,
            ),
            KeyDef {
                hint: Some("L5"),
                ..mod_key(
                    "AltGr",
                    K::KEY_RIGHTALT,
                    Behavior::AltGr,
                    0.9,
                    Align::Center,
                    VAlign::Center,
                )
            },
            KeyDef {
                ..mod_key(
                    "Win",
                    K::KEY_LEFTMETA,
                    Behavior::Win,
                    0.9,
                    Align::Center,
                    VAlign::Center,
                )
            },
            KeyDef {
                width_weight: 4.0,
                align: Align::Left,
                hint: Some("Y"),
                ..key("", None, K::KEY_SPACE)
            },
            KeyDef {
                symbol_font: true,
                font_size: Some(17.0),
                shift_keycode: Some(K::KEY_UP),
                ..key("\u{25C0}", Some("\u{25B2}"), K::KEY_LEFT)
            },
            KeyDef {
                symbol_font: true,
                font_size: Some(17.0),
                shift_keycode: Some(K::KEY_DOWN),
                ..key("\u{25B6}", Some("\u{25BC}"), K::KEY_RIGHT)
            },
            KeyDef {
                shifted: Some("Copy"),
                ..mod_key(
                    "Paste",
                    0,
                    Behavior::Paste,
                    1.2,
                    Align::Center,
                    VAlign::Center,
                )
            },
            KeyDef {
                glyph: Some("\u{2328}"), // ⌨
                ..mod_key("Close", 0, Behavior::Move, 1.4, Align::Center, Top)
            },
        ],
    ]
}

#[derive(Clone, Copy, Debug)]
pub struct KeyLayout {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub row: usize,
    pub col: usize,
}

pub struct VirtualKeyboard {
    pub keys: Vec<Vec<KeyDef>>,
    pub key_rows: usize,
    key_width: Vec<f32>,
    pub key_height: f32,
}

impl VirtualKeyboard {
    pub fn new(keys: Vec<Vec<KeyDef>>, width: f32, height: f32) -> Self {
        let mut kb = Self {
            keys,
            key_rows: 0,
            key_width: Vec::new(),
            key_height: 0.0,
        };
        kb.key_rows = kb.keys.len();
        kb.update_dimensions(width, height);
        kb
    }

    fn uniform_key_width(&self, row: usize, width: f32) -> f32 {
        let unpadded =
            width - PADDING_OUTER * 2.0 - (self.keys[row].len() as f32 * PADDING_INNER * 2.0);
        let weights_total: f32 = self.keys[row].iter().map(|k| k.width_weight).sum();
        unpadded / weights_total
    }

    pub fn update_dimensions(&mut self, width: f32, height: f32) {
        self.key_height =
            (height - PADDING_OUTER * 2.0 - (self.key_rows as f32 * PADDING_INNER * 2.0))
                / self.key_rows as f32;
        self.key_width = (0..self.key_rows)
            .map(|r| self.uniform_key_width(r, width))
            .collect();
    }

    fn find_key_row(&self, y_coord: f32) -> usize {
        ((y_coord - PADDING_OUTER) / (self.key_height + PADDING_INNER * 2.0)) as usize
    }

    /// Key under an absolute pixel coord, or None.
    pub fn find_key(&self, x_coord: f32, y_coord: f32) -> Option<(usize, usize)> {
        let i_row = self.find_key_row(y_coord).min(self.key_rows - 1);
        let mut iterated_x = PADDING_OUTER;
        for (i_key, key) in self.keys[i_row].iter().enumerate() {
            iterated_x += key.width_weight * self.key_width[i_row] + PADDING_INNER * 2.0;
            if x_coord < iterated_x {
                return Some((i_row, i_key));
            }
        }
        None
    }

    pub fn get_key_layout(&self, target_row: usize, target_col: usize) -> Option<KeyLayout> {
        self.all_key_layouts()
            .into_iter()
            .find(|l| l.row == target_row && l.col == target_col)
    }

    /// Column in `target_row` whose pixel range covers `x`, else closest by
    /// center (used by UP/DOWN stepping).
    pub fn find_col_at_x(&self, target_row: usize, x: f32) -> Option<usize> {
        if target_row >= self.key_rows {
            return None;
        }
        let layouts: Vec<KeyLayout> = self
            .all_key_layouts()
            .into_iter()
            .filter(|l| l.row == target_row)
            .collect();
        if layouts.is_empty() {
            return None;
        }
        for l in &layouts {
            if l.x <= x && x < l.x + l.w {
                return Some(l.col);
            }
        }
        let mut best = layouts[0].col;
        let mut best_dist = (layouts[0].x + layouts[0].w / 2.0 - x).abs();
        for l in &layouts[1..] {
            let d = (l.x + l.w / 2.0 - x).abs();
            if d < best_dist {
                best = l.col;
                best_dist = d;
            }
        }
        Some(best)
    }

    pub fn rows(&self) -> usize {
        self.key_rows
    }

    pub fn row_len(&self, row: usize) -> usize {
        self.keys[row].len()
    }

    /// Displayed label for a key given shift/caps state.
    pub fn display_label(&self, key: &KeyDef, shift_held: bool, caps_on: bool) -> &str {
        let Some(shifted) = key.shifted else {
            return key.label;
        };
        let is_letter = key.label.len() == 1 && key.label.chars().next().unwrap().is_alphabetic();
        if is_letter {
            if shift_held ^ caps_on {
                shifted
            } else {
                key.label
            }
        } else if shift_held {
            shifted
        } else {
            key.label
        }
    }

    /// True if holding this key should auto-repeat. EVERY plain typing key
    /// repeats (letters, digits, symbols, Space, Tab, Enter) — the old
    /// Steam-OSK rule (Backspace + arrows only) silently dropped letter
    /// repeats at the drain step ("no repeat for letters"). Modifier and
    /// special behaviors toggle instead of repeating.
    pub fn is_repeatable(&self, row: usize, col: usize) -> bool {
        let Some(key) = self.keys.get(row).and_then(|r| r.get(col)) else {
            return false;
        };
        key.behavior == Behavior::Generic && key.keycode != 0
    }

    /// Step the DPAD/stick cursor using actual layout pixel positions.
    pub fn step_cursor(&self, state: &OskState, direction: Dir, _haptic: bool) {
        let start = state.get_cursor();
        let mut row = start.0;
        let mut col = start.1;
        let rows = self.key_rows;
        if row >= rows {
            row = rows - 1;
            col = 0;
        }

        match direction {
            Dir::Left => {
                if col > 0 {
                    col -= 1;
                } else {
                    col = self.keys[row].len() - 1; // wrap to last key
                }
            }
            Dir::Right => {
                if col < self.keys[row].len() - 1 {
                    col += 1;
                } else {
                    col = 0; // wrap to first key
                }
            }
            Dir::Up | Dir::Down => {
                let new_row = if direction == Dir::Up {
                    row.wrapping_sub(1)
                } else {
                    row + 1
                };
                if new_row < rows {
                    if let Some(cur_layout) = self.get_key_layout(row, col) {
                        let x_center = cur_layout.x + cur_layout.w / 2.0;
                        if let Some(new_col) = self.find_col_at_x(new_row, x_center) {
                            col = new_col;
                        }
                    }
                    row = new_row;
                }
            }
        }

        col = col.min(self.keys[row].len() - 1);
        state.set_cursor(row, col);
    }
}

// ---------------------------------------------------------------------------
// Key dispatch (vkb.dispatch_key + the on_key_* callbacks).
// ---------------------------------------------------------------------------

pub struct Dispatcher<'a> {
    pub kb: &'a Keyboard,
    pub state: &'a OskState,
}

impl Dispatcher<'_> {
    pub fn toggle_shift(&self) {
        let new = !self.state.is_shift_latched();
        self.state.set_shift_latched(new);
        if new {
            self.kb.press_code(K::KEY_LEFTSHIFT);
            let mut hl = std::collections::HashSet::new();
            hl.insert(K::KEY_LEFTSHIFT);
            hl.insert(K::KEY_RIGHTSHIFT);
            self.state.set_highlighted(hl);
        } else {
            self.kb.release_code(K::KEY_LEFTSHIFT);
            self.state.set_highlighted(std::collections::HashSet::new());
        }
        self.state.set_shift_held(new);
    }

    /// Sticky Ctrl — presses the REAL KEY_LEFTCTRL until toggled off or the
    /// OSK closes (release_held clears it), so the next key tap arrives as
    /// Ctrl+key. Mirrors toggle_shift exactly.
    pub fn toggle_ctrl(&self) {
        let new = !self.state.is_ctrl_latched();
        self.state.set_ctrl_latched(new);
        if new {
            self.kb.press_code(K::KEY_LEFTCTRL);
            let mut hl = std::collections::HashSet::new();
            hl.insert(K::KEY_LEFTCTRL);
            self.state.set_highlighted(hl);
        } else {
            self.kb.release_code(K::KEY_LEFTCTRL);
            self.state.set_highlighted(std::collections::HashSet::new());
        }
    }

    /// Sticky Alt — same mechanism as toggle_ctrl.
    pub fn toggle_alt(&self) {
        let new = !self.state.is_alt_latched();
        self.state.set_alt_latched(new);
        if new {
            self.kb.press_code(K::KEY_LEFTALT);
            let mut hl = std::collections::HashSet::new();
            hl.insert(K::KEY_LEFTALT);
            self.state.set_highlighted(hl);
        } else {
            self.kb.release_code(K::KEY_LEFTALT);
            self.state.set_highlighted(std::collections::HashSet::new());
        }
    }

    /// Sticky AltGr (Right Alt) — Polish diacritics on the programmer layout.
    pub fn toggle_altgr(&self) {
        let new = !self.state.is_altgr_latched();
        self.state.set_altgr_latched(new);
        if new {
            self.kb.press_code(K::KEY_RIGHTALT);
            let mut hl = std::collections::HashSet::new();
            hl.insert(K::KEY_RIGHTALT);
            self.state.set_highlighted(hl);
        } else {
            self.kb.release_code(K::KEY_RIGHTALT);
            self.state.set_highlighted(std::collections::HashSet::new());
        }
    }

    /// Sticky Win/Super.
    pub fn toggle_win(&self) {
        let new = !self.state.is_win_latched();
        self.state.set_win_latched(new);
        if new {
            self.kb.press_code(K::KEY_LEFTMETA);
            let mut hl = std::collections::HashSet::new();
            hl.insert(K::KEY_LEFTMETA);
            self.state.set_highlighted(hl);
        } else {
            self.kb.release_code(K::KEY_LEFTMETA);
            self.state.set_highlighted(std::collections::HashSet::new());
        }
    }

    /// Release every sticky modifier (latch + real OS key) — used so Enter
    /// never arrives as a modifier chord.
    pub fn drop_latches(&self) {
        if self.state.is_shift_latched() {
            self.state.set_shift_latched(false);
            self.state.set_shift_held(false);
            self.kb.release_code(K::KEY_LEFTSHIFT);
            self.kb.release_code(K::KEY_RIGHTSHIFT);
        }
        if self.state.is_ctrl_latched() {
            self.state.set_ctrl_latched(false);
            self.kb.release_code(K::KEY_LEFTCTRL);
        }
        if self.state.is_alt_latched() {
            self.state.set_alt_latched(false);
            self.kb.release_code(K::KEY_LEFTALT);
        }
        if self.state.is_altgr_latched() {
            self.state.set_altgr_latched(false);
            self.kb.release_code(K::KEY_RIGHTALT);
        }
        if self.state.is_win_latched() {
            self.state.set_win_latched(false);
            self.kb.release_code(K::KEY_LEFTMETA);
        }
    }

    /// Force-release the OS Shift key so hiding the keyboard never leaves
    /// Shift stuck down.
    pub fn release_shift(&self) {
        self.kb.release_code(K::KEY_LEFTSHIFT);
        self.kb.release_code(K::KEY_RIGHTSHIFT);
        self.state.set_shift_latched(false);
        self.state.set_shift_held(false);
        self.state.set_highlighted(std::collections::HashSet::new());
    }

    pub fn dispatch(&self, key: &KeyDef) {
        // Keys with a shift_keycode (◀▶ -> ▲▼) send the alternate keycode
        // WITHOUT the OS seeing Shift (Shift+Arrow would select text).
        let use_shift_key = key.shift_keycode.is_some() && self.state.is_shift_held();
        let keycode = if use_shift_key {
            key.shift_keycode.unwrap()
        } else {
            key.keycode
        };
        if use_shift_key {
            self.kb.release_code(K::KEY_LEFTSHIFT);
        }
        match key.behavior {
            Behavior::Generic => {
                // Enter must arrive CLEAN: any sticky modifier riding along
                // turns it into Shift/Ctrl/Alt/Win+Enter, which apps treat as
                // newline/submit/fullscreen — the "enter is kinda broken"
                // reports. Drop every latch for this one key.
                if keycode == K::KEY_ENTER {
                    self.drop_latches();
                }
                self.kb.tap_code(keycode);
                // Caps Lock state is read live from the X server modifier
                // mask by the renderer — injected capslock shows up on the
                // next frame with no manual tracking.
            }
            Behavior::Shift => self.toggle_shift(),
            Behavior::Ctrl => self.toggle_ctrl(),
            Behavior::Alt => self.toggle_alt(),
            Behavior::AltGr => self.toggle_altgr(),
            Behavior::Win => self.toggle_win(),
            Behavior::Paste => {
                // Shift -> Ctrl+C (copy), otherwise Ctrl+V (paste). Always
                // release both shifts first so the chord isn't polluted by a
                // stranded modifier.
                let shift_held = self.state.is_shift_held();
                self.kb.release_code(K::KEY_LEFTSHIFT);
                self.kb.release_code(K::KEY_RIGHTSHIFT);
                self.kb.press_code(K::KEY_LEFTCTRL);
                self.kb
                    .press_code(if shift_held { K::KEY_C } else { K::KEY_V });
                self.kb
                    .release_code(if shift_held { K::KEY_C } else { K::KEY_V });
                self.kb.release_code(K::KEY_LEFTCTRL);
                if shift_held {
                    self.kb.press_code(K::KEY_LEFTSHIFT);
                }
            }
            Behavior::Emoji => {
                // Win+. opens the OS emoji picker; a second press sends Escape
                // to dismiss it. Shifts released first (Meta+Shift+. is a
                // different shortcut).
                let shift_held = self.state.is_shift_held();
                self.kb.release_code(K::KEY_LEFTSHIFT);
                self.kb.release_code(K::KEY_RIGHTSHIFT);
                if self.state.is_emoji_open() {
                    self.kb.tap_code(K::KEY_ESC);
                    self.state.set_emoji_open(false);
                } else {
                    self.kb.press_code(K::KEY_LEFTMETA);
                    self.kb.press_code(K::KEY_DOT);
                    self.kb.release_code(K::KEY_DOT);
                    self.kb.release_code(K::KEY_LEFTMETA);
                    self.state.set_emoji_open(true);
                }
                if shift_held {
                    self.kb.press_code(K::KEY_LEFTSHIFT);
                }
            }
            Behavior::Move => {
                // Unshifted Move closes the keyboard (Shift+Move cycles the
                // window position in the Python app — position cycle isn't
                // ported yet, so it just closes too).
                self.state.close();
            }
        }
        if use_shift_key && self.state.is_shift_held() {
            self.kb.press_code(K::KEY_LEFTSHIFT);
        }
    }
}
