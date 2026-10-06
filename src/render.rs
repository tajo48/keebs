//! OSK window + renderer: SDL3 window (borderless, transparent, no-focus,
//! always-on-top) anchored bottom-center, TTF text, key grid painting.
//!
//! Rust port of `adusk/screen.py` (lean v1: built-in DefaultTheme palette,
//! static dual labels instead of the slide/fade animation, no glyph PNGs —
//! those come with the skins/tray phases).

use std::collections::HashMap;
use std::ffi::CString;
use std::thread;
use std::time::{Duration, Instant};

use crate::osk_state::{InputState, OskState};
use crate::sdl3::{self, SDL_Color, SDL_FRect, sdl};
use crate::vkb::{Align, KeyDef, KeyLayout, VAlign, VirtualKeyboard};

// Steam Big Picture palette (screen.py class defaults — the DefaultTheme
// fallbacks).
const BG: (u8, u8, u8) = (0x23, 0x26, 0x2E);
const KEY_INACTIVE: (u8, u8, u8) = (0x0E, 0x14, 0x1B);
const KEY_HOVER: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);
const KEY_CLICK: (u8, u8, u8) = (0x1A, 0x9F, 0xFF);
const MODIFIER_IDLE: (u8, u8, u8) = (0x00, 0x00, 0x00);
const TEXT_INACTIVE: (u8, u8, u8) = (0xEE, 0xF3, 0xF7);
const TEXT_HOVER: (u8, u8, u8) = (0x0E, 0x14, 0x1B);
const TEXT_CLICK: (u8, u8, u8) = (0xEE, 0xF3, 0xF7);
const KEY_BEVEL: (u8, u8, u8) = (0x4A, 0x5D, 0x70);
const SHADOW_LABEL: (u8, u8, u8) = (120, 123, 127);

const BASE_HEIGHT: f32 = 369.0;

pub struct Screen {
    pub window: sdl3::SdlWindow,
    pub renderer: sdl3::SdlRenderer,
    font_main: sdl3::TtfFont,
    font_small: sdl3::TtfFont,
    font_shadow: sdl3::TtfFont,
    font_symbol: sdl3::TtfFont,
    font_symbol_small: sdl3::TtfFont,
    font_scale: f32,
    /// Texture cache: (font_size, symbol, color, label) -> (tex, w, h).
    tex_cache: HashMap<(i32, bool, (u8, u8, u8), String), (sdl3::SdlTexture, i32, i32)>,
}

fn font_candidates() -> Vec<String> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let data = format!("{manifest}/../linux/data/fonts");
    let mut v = vec![
        format!("{data}/Selawik-Semibold.ttf"),
        format!("{data}/DejaVuSansCondensed-Bold.ttf"),
        // System fallbacks (screen.py lists).
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf".to_string(),
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf".to_string(),
        "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf".to_string(),
    ];
    // Symbol font: bundled DejaVu covers the geometric arrows.
    v.push(format!("{data}/DejaVuSansCondensed-Bold.ttf"));
    v.push("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".to_string());
    v
}

fn first_existing(paths: &[String]) -> Option<String> {
    paths
        .iter()
        .find(|p| std::path::Path::new(p).is_file())
        .cloned()
}

impl Screen {
    pub fn new(state: &OskState) -> Self {
        let s = sdl();
        unsafe {
            // Never auto-activate when shown: opening the OSK must not steal
            // focus from the user's target app.
            let hint = CString::new("SDL_WINDOW_ACTIVATE_WHEN_SHOWN").unwrap();
            (s.set_hint)(hint.as_ptr(), b"0\0".as_ptr() as *const _);

            // Anchor to the bottom-center of the primary display's usable
            // area (above the taskbar).
            let disp = (s.get_primary_display)();
            let mut bounds = sdl3::SDL_Rect::default();
            let (mut win_x, mut win_y) =
                (sdl3::SDL_WINDOWPOS_CENTERED, sdl3::SDL_WINDOWPOS_CENTERED);
            let w = state.abs_dims().0 as i32;
            let h = state.abs_dims().1 as i32;
            if disp != 0 && (s.get_display_usable_bounds)(disp, &mut bounds) {
                win_x = bounds.x + ((bounds.w - w) / 2).max(0);
                win_y = bounds.y + (bounds.h - h).max(0);
            }

            let title = CString::new("").unwrap();
            let base_flags = sdl3::SDL_WINDOW_BORDERLESS
                | sdl3::SDL_WINDOW_HIDDEN
                | sdl3::SDL_WINDOW_UTILITY
                | sdl3::SDL_WINDOW_ALWAYS_ON_TOP
                | sdl3::SDL_WINDOW_NOT_FOCUSABLE;
            let window = (s.create_window)(
                title.as_ptr(),
                w,
                h,
                base_flags | sdl3::SDL_WINDOW_TRANSPARENT,
            );
            let window = if !window.is_null() {
                window
            } else {
                (s.create_window)(title.as_ptr(), w, h, base_flags)
            };
            assert!(
                !window.is_null(),
                "SDL_CreateWindow failed: {}",
                s.get_error_string()
            );
            (s.set_window_position)(window, win_x, win_y);

            let renderer = (s.create_renderer)(window, std::ptr::null());
            assert!(
                !renderer.is_null(),
                "SDL_CreateRenderer failed: {}",
                s.get_error_string()
            );
            (s.set_render_draw_blend_mode)(renderer, sdl3::SDL_BLENDMODE_BLEND);

            let paths = font_candidates();
            let font_path =
                first_existing(&paths[..paths.len() - 2]).unwrap_or_else(|| paths[1].clone());
            let sym_path = paths[paths.len() - 2].clone();
            println!("osk: font `{font_path}`");

            let font_scale = BASE_HEIGHT / 369.0 * (h as f32 / BASE_HEIGHT);
            let open_font = |path: &str, size: f32| -> sdl3::TtfFont {
                let c = CString::new(path).unwrap();
                let font = (s.ttf_open_font)(c.as_ptr(), size * font_scale);
                assert!(
                    !font.is_null(),
                    "TTF_OpenFont failed for {path}: {}",
                    s.get_error_string()
                );
                font
            };
            (s.ttf_init)();
            let font_main = open_font(&font_path, 26.0);
            let font_small = open_font(&font_path, 20.0);
            let font_shadow = open_font(&font_path, 21.0);
            let font_symbol = open_font(&sym_path, 26.0);
            let font_symbol_small = open_font(&sym_path, 20.0);

            Self {
                window,
                renderer,
                font_main,
                font_small,
                font_shadow,
                font_symbol,
                font_symbol_small,
                font_scale,
                tex_cache: HashMap::new(),
            }
        }
    }

    pub fn show(&self) {
        unsafe {
            (sdl().show_window)(self.window);
        }
    }

    pub fn hide(&self) {
        unsafe {
            (sdl().hide_window)(self.window);
        }
    }

    fn color(&self, c: (u8, u8, u8)) -> SDL_Color {
        SDL_Color {
            r: c.0,
            g: c.1,
            b: c.2,
            a: 255,
        }
    }

    /// Cached text texture (labels are a small fixed set; never evicted).
    fn text_texture(
        &mut self,
        label: &str,
        font_size: f32,
        symbol: bool,
        color: (u8, u8, u8),
    ) -> Option<(sdl3::SdlTexture, i32, i32)> {
        let key = (
            (font_size * self.font_scale) as i32,
            symbol,
            color,
            label.to_string(),
        );
        if let Some(v) = self.tex_cache.get(&key) {
            return Some(*v);
        }
        let font = match (symbol, key.0 > (21.0 * self.font_scale) as i32) {
            (true, _) => {
                if font_size <= 21.0 {
                    self.font_symbol_small
                } else {
                    self.font_symbol
                }
            }
            (false, true) => self.font_main,
            (false, false) => {
                if font_size == 21.0 {
                    self.font_shadow
                } else {
                    self.font_small
                }
            }
        };
        // Use the exact size font when it's a custom (non-standard) size.
        let font = if font_size != 26.0 && font_size != 20.0 && font_size != 21.0 && !symbol {
            // One-off custom size (e.g. YAML font_size: 17): fall back to the
            // closest of our two text fonts rather than opening a new font.
            if font_size <= 21.0 {
                self.font_small
            } else {
                self.font_main
            }
        } else {
            font
        };
        let tex = sdl3::render_text_texture(self.renderer, font, label, self.color(color))?;
        self.tex_cache.insert(key, tex);
        Some(tex)
    }

    fn fill(&self, rect: SDL_FRect, color: (u8, u8, u8)) {
        unsafe {
            let s = sdl();
            (s.set_render_draw_color)(self.renderer, color.0, color.1, color.2, 255);
            (s.render_fill_rect)(self.renderer, &rect);
        }
    }

    /// Clear to the background and paint every key.
    pub fn render(&mut self, state: &OskState, vkb: &VirtualKeyboard) {
        let s = sdl();
        unsafe {
            (s.set_render_draw_color)(self.renderer, BG.0, BG.1, BG.2, 255);
            (s.render_clear)(self.renderer);
        }

        let shift_held = state.is_shift_held();
        let caps_on = state.is_caps_on();
        let highlighted = state.get_highlighted();
        let lpad_touched = state.is_lpad_touched();
        let rpad_touched = state.is_rpad_touched();
        let (cursor_row, cursor_col) = state.get_cursor();
        let mouse_press_cell = state.get_mouse_press_cell();
        let pointers = state.get_pointers();

        for key in vkb.all_key_layouts() {
            let mut input_state = InputState::Inactive;
            for ptr in &pointers {
                if ptr.in_box(key.x, key.y, key.w, key.h) && ptr.state > input_state {
                    input_state = ptr.state;
                }
            }
            let kd = &vkb.keys[key.row][key.col];
            if kd.keycode != 0 && highlighted.contains(&kd.keycode) {
                input_state = InputState::Click;
            }
            // DPAD cursor shows as hover (hidden while pads are touched).
            if key.row == cursor_row && key.col == cursor_col && !lpad_touched && !rpad_touched {
                input_state = InputState::Hover.max(input_state);
            }
            if let Some((pr, pc)) = mouse_press_cell {
                if pr == key.row && pc == key.col {
                    input_state = InputState::Click;
                }
            }

            // Fill: modifiers idle -> black; hover -> white; press -> blue.
            let fill = match input_state {
                InputState::Click => KEY_CLICK,
                InputState::Hover => KEY_HOVER,
                InputState::Inactive => {
                    if kd.modifier {
                        MODIFIER_IDLE
                    } else {
                        KEY_INACTIVE
                    }
                }
            };
            let rect = SDL_FRect {
                x: key.x,
                y: key.y,
                w: key.w,
                h: key.h,
            };
            self.fill(rect, fill);
            // Subtle top-edge bevel on idle keys.
            if input_state == InputState::Inactive {
                unsafe {
                    (s.set_render_draw_color)(
                        self.renderer,
                        KEY_BEVEL.0,
                        KEY_BEVEL.1,
                        KEY_BEVEL.2,
                        255,
                    );
                    (s.render_line)(
                        self.renderer,
                        key.x + 1.0,
                        key.y + 0.5,
                        key.x + key.w - 1.0,
                        key.y + 0.5,
                    );
                }
            }

            let text_color = match input_state {
                InputState::Click => TEXT_CLICK,
                InputState::Hover => TEXT_HOVER,
                InputState::Inactive => TEXT_INACTIVE,
            };

            let is_letter =
                kd.label.len() == 1 && kd.label.chars().next().unwrap_or(' ').is_alphabetic();
            let dual_eligible = kd.shifted.is_some() && !is_letter;
            let label = vkb.display_label(kd, shift_held, caps_on);

            if dual_eligible && !shift_held {
                // Static v1 of the dual-label slide: shifted form as a small
                // grey shadow above, main label at the bottom.
                if let Some(shifted) = kd.shifted {
                    self.draw_label(
                        shifted,
                        kd,
                        &key,
                        SHADOW_LABEL,
                        Align::Center,
                        VAlign::Top,
                        21.0,
                    );
                }
                self.draw_label(label, kd, &key, text_color, kd.align, VAlign::Bottom, 21.0);
            } else {
                let size = kd
                    .font_size
                    .unwrap_or(if kd.modifier { 20.0 } else { 26.0 });
                self.draw_label(label, kd, &key, text_color, kd.align, kd.valign, size);
            }
        }

        unsafe {
            (s.render_present)(self.renderer);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_label(
        &mut self,
        label: &str,
        kd: &KeyDef,
        key: &KeyLayout,
        color: (u8, u8, u8),
        align: Align,
        valign: VAlign,
        font_size: f32,
    ) {
        if label.is_empty() {
            return;
        }
        let Some((tex, tw, th)) = self.text_texture(label, font_size, kd.symbol_font, color) else {
            return;
        };
        let x = match align {
            Align::Left => key.x + 8.0,
            Align::Right => key.x + key.w - tw as f32 - 8.0,
            Align::Center => key.x + (key.w - tw as f32) / 2.0,
        };
        let y = match valign {
            VAlign::Top => key.y + 3.0,
            VAlign::Bottom => key.y + key.h - th as f32 - 4.0,
            VAlign::Center => key.y + (key.h - th as f32) / 2.0,
        };
        let dst = SDL_FRect {
            x,
            y,
            w: tw as f32,
            h: th as f32,
        };
        unsafe {
            (sdl().render_texture)(self.renderer, tex, std::ptr::null(), &dst);
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        unsafe {
            let s = sdl();
            (s.destroy_renderer)(self.renderer);
            (s.destroy_window)(self.window);
        }
    }
}

/// Frame pacer (screen.py `_FrameLimiter`): resyncs to now after a long
/// frame instead of bursting.
pub struct FrameLimiter {
    interval: Duration,
    next: Instant,
}

impl FrameLimiter {
    pub fn new(fps: f32) -> Self {
        Self {
            interval: Duration::from_secs_f32(1.0 / fps),
            next: Instant::now(),
        }
    }

    pub fn set_fps(&mut self, fps: f32) {
        self.interval = Duration::from_secs_f32(1.0 / fps);
    }

    pub fn delay(&mut self) {
        self.next += self.interval;
        let now = Instant::now();
        if self.next > now {
            thread::sleep(self.next - now);
        } else {
            self.next = now;
        }
    }
}
