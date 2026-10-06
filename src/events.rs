//! Minimal EventMapper replacement matching only the surface adusk uses:
//! `set_button_action(button, key)` and `process(sci)` diffing the button
//! bitmask against the previous frame.
//!
//! Rust port of `linux/steamcontroller/events.py`.

use crate::sc::{SC_STATUS_INPUT, SCI_NULL, SciInput};
use crate::uinput::Keyboard;

pub struct EventMapper {
    kb: Keyboard,
    /// (button mask, KEY_* keycode name)
    map: Vec<(u32, String)>,
    prev: SciInput,
}

impl EventMapper {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            kb: Keyboard::new()?,
            map: Vec::new(),
            prev: SCI_NULL,
        })
    }

    pub fn set_button_action(&mut self, button: u32, key: &str) {
        self.map.retain(|(mask, _)| *mask != button);
        self.map.push((button, key.to_string()));
    }

    pub fn process(&mut self, sci: &SciInput) {
        if sci.status != SC_STATUS_INPUT {
            return;
        }
        let xor = self.prev.buttons ^ sci.buttons;
        let newly_pressed = xor & sci.buttons;
        let newly_released = xor & self.prev.buttons;

        for (mask, key) in &self.map {
            if mask & newly_pressed != 0 {
                self.kb.press_event(&[key.as_str()]);
            } else if mask & newly_released != 0 {
                self.kb.release_event(&[key.as_str()]);
            }
        }
        self.prev = *sci;
    }
}
