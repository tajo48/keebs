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

// Catppuccin Frappé palette, blue accent (https://catppuccin.com/palette).
const BG: (u8, u8, u8) = (0x29, 0x2C, 0x3C); // mantle
const KEY_INACTIVE: (u8, u8, u8) = (0x41, 0x45, 0x59); // surface0
const KEY_HOVER: (u8, u8, u8) = (0x8C, 0xAA, 0xEE); // blue
const KEY_CLICK: (u8, u8, u8) = (0x85, 0xC1, 0xDC); // sapphire
const MODIFIER_IDLE: (u8, u8, u8) = (0x23, 0x26, 0x34); // crust
const TEXT_INACTIVE: (u8, u8, u8) = (0xC6, 0xD3, 0xF5); // text
const TEXT_HOVER: (u8, u8, u8) = (0x29, 0x2C, 0x3C); // mantle (dark on blue)
const TEXT_CLICK: (u8, u8, u8) = (0x23, 0x26, 0x34); // crust
const KEY_BEVEL: (u8, u8, u8) = (0x51, 0x57, 0x6D); // surface1
const SHADOW_LABEL: (u8, u8, u8) = (0x73, 0x79, 0x94); // overlay0

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

/// Directories to search for the bundled fonts, in order:
/// 1. $KEEBS_FONT_DIR (set by the nix wrapper to $out/share/keebs/fonts)
/// 2. next to the binary (nix store layout: ../share/keebs/fonts, ./fonts)
/// 3. the dev checkout's bundled copy (keebs/fonts, linux/data/fonts)
fn font_dirs() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(dir) = std::env::var("KEEBS_FONT_DIR") {
        v.push(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(format!("{}/../share/keebs/fonts", dir.display()));
            v.push(format!("{}/fonts", dir.display()));
        }
    }
    v.push(format!("{}/fonts", env!("CARGO_MANIFEST_DIR")));
    v.push(format!(
        "{}/../linux/data/fonts",
        env!("CARGO_MANIFEST_DIR")
    ));
    v
}

fn find_font(dirs: &[String], filename: &str) -> Option<String> {
    dirs.iter()
        .map(|d| format!("{d}/{filename}"))
        .find(|p| std::path::Path::new(p).is_file())
}

/// (main font path, symbol font path). Selawik Semibold is the Steam
/// Big-Picture-look font; DejaVu covers the geometric arrows. System fonts
/// are fallbacks only (screen.py's candidate lists).
fn resolve_fonts() -> (String, String) {
    let dirs = font_dirs();
    let system_main = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    ];
    let system_sym = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ];
    let main = find_font(&dirs, "Selawik-Semibold.ttf")
        .or_else(|| find_font(&dirs, "DejaVuSansCondensed-Bold.ttf"))
        .or_else(|| {
            system_main
                .iter()
                .find(|p| std::path::Path::new(p).is_file())
                .map(|p| p.to_string())
        })
        .unwrap_or_else(|| "Selawik-Semibold.ttf".to_string());
    let sym = find_font(&dirs, "DejaVuSansCondensed-Bold.ttf")
        .or_else(|| {
            system_sym
                .iter()
                .find(|p| std::path::Path::new(p).is_file())
                .map(|p| p.to_string())
        })
        .unwrap_or_else(|| main.clone());
    (main, sym)
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

            // Exactly the Python flags: BORDERLESS | HIDDEN | TRANSPARENT,
            // plus SDL3's real SDL_WINDOW_ALWAYS_ON_TOP (0x10000 — the sdl3w
            // value 0x100 is MOUSE_GRABBED and must never be passed). No
            // UTILITY/NOT_FOCUSABLE — on real SDL3 those values collide with
            // TOOLTIP/POPUP_MENU, which demand a parent window. The no-focus
            // behavior comes from runtime calls (activate-when-shown hint,
            // X11 WM hints), like adusk.py.
            let title = CString::new("").unwrap();
            let base_flags = sdl3::SDL_WINDOW_BORDERLESS
                | sdl3::SDL_WINDOW_HIDDEN
                | sdl3::SDL_WINDOW_ALWAYS_ON_TOP;
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
            // X11/XWayland: never take keyboard focus (WM_HINTS.input=False),
            // so clicks/typing keep landing in the app behind the OSK.
            let x11 = sdl3::window_is_x11(window);
            println!(
                "osk: window backend: {}{}",
                if x11 {
                    "x11/xwayland"
                } else {
                    "wayland (native — topmost/no-focus unavailable!)"
                },
                if x11 {
                    ""
                } else {
                    " — set SDL_VIDEODRIVER=x11"
                },
            );
            if x11 {
                sdl3::make_window_no_focus(window);
            }

            let renderer = (s.create_renderer)(window, std::ptr::null());
            assert!(
                !renderer.is_null(),
                "SDL_CreateRenderer failed: {}",
                s.get_error_string()
            );
            (s.set_render_draw_blend_mode)(renderer, sdl3::SDL_BLENDMODE_BLEND);

            let (font_path, sym_path) = resolve_fonts();
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
            let s = sdl();
            (s.show_window)(self.window);
            // Flip topmost AFTER mapping: WMs can ignore state changes on
            // unmapped windows, so setting it before show only sticks on the
            // next re-assert tick (the "reopen buried" bug).
            (s.set_window_always_on_top)(self.window, true);
            // Direct restack so it's on top the instant it maps, and re-apply
            // the no-focus hints in case the WM reset them at map time.
            sdl3::raise_window(self.window);
            sdl3::make_window_no_focus(self.window);
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
                if ptr.state == InputState::Inactive {
                    continue;
                }
                // Pointer coords are stored as fractions of the window; keys
                // are laid out in pixels — compare in pixel space (this was
                // the dead-touchpad bug: fracs were compared to px rects).
                let (px, py) = state.frac_to_abs(ptr.frac);
                if px >= key.x && py >= key.y && px <= key.x + key.w && py <= key.y + key.h {
                    if ptr.state > input_state {
                        input_state = ptr.state;
                    }
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

            // Button-hint glyph (Unicode stand-in for the Python PNG icons),
            // drawn on the opposite side from the label.
            if let Some(glyph) = kd.glyph {
                let (ga, gv) = match kd.align {
                    Align::Left => (Align::Right, VAlign::Center),
                    Align::Right => (Align::Left, VAlign::Center),
                    Align::Center => (Align::Center, VAlign::Center),
                };
                self.draw_symbol(glyph, &key, text_color, ga, gv);
            }
        }

        // Pointer dots: show where each finger is on the trackpads
        // (screen.py render_ptr). Clicking pads paint the dot blue.
        for ptr in &pointers {
            if ptr.state == InputState::Inactive {
                continue;
            }
            let (cx, cy) = state.frac_to_abs(ptr.frac);
            let (color, alpha) = if ptr.state == InputState::Click {
                (KEY_CLICK, 220u8)
            } else {
                (KEY_HOVER, 150u8)
            };
            self.fill_circle(cx, cy, 13.0, color, alpha);
        }

        unsafe {
            (s.render_present)(self.renderer);
        }
    }

    /// Filled circle via SDL_RenderGeometry (triangle fan).
    fn fill_circle(&self, cx: f32, cy: f32, r: f32, color: (u8, u8, u8), alpha: u8) {
        const SEGMENTS: usize = 28;
        // SDL3 vertex colors are float RGBA (SDL_FColor), not bytes.
        let fcolor = sdl3::SDL_FColor {
            r: color.0 as f32 / 255.0,
            g: color.1 as f32 / 255.0,
            b: color.2 as f32 / 255.0,
            a: alpha as f32 / 255.0,
        };
        let vtx = |x: f32, y: f32| sdl3::SDL_Vertex {
            position: sdl3::SDL_FPoint { x, y },
            color: fcolor,
            tex_coord: sdl3::SDL_FPoint { x: 0.0, y: 0.0 },
        };
        let mut verts = Vec::with_capacity(SEGMENTS + 2);
        verts.push(vtx(cx, cy));
        for i in 0..=SEGMENTS {
            let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            verts.push(vtx(cx + a.cos() * r, cy + a.sin() * r));
        }
        let mut indices = Vec::with_capacity(SEGMENTS * 3);
        for i in 0..SEGMENTS {
            indices.extend_from_slice(&[0, (i + 1) as i32, (i + 2) as i32]);
        }
        unsafe {
            (sdl().render_geometry)(
                self.renderer,
                std::ptr::null_mut(),
                verts.as_ptr(),
                (SEGMENTS + 2) as i32,
                indices.as_ptr(),
                (SEGMENTS * 3) as i32,
            );
        }
    }

    fn draw_symbol(
        &mut self,
        glyph: &str,
        key: &KeyLayout,
        color: (u8, u8, u8),
        align: Align,
        valign: VAlign,
    ) {
        if glyph.is_empty() {
            return;
        }
        let cache_key = (18, true, color, glyph.to_string());
        let tex = match self.tex_cache.get(&cache_key) {
            Some(v) => *v,
            None => {
                let Some(tex) = sdl3::render_text_texture(
                    self.renderer,
                    self.font_symbol_small,
                    glyph,
                    self.color(color),
                ) else {
                    return;
                };
                self.tex_cache.insert(cache_key, tex);
                tex
            }
        };
        let (tw, th) = (tex.1 as f32, tex.2 as f32);
        let x = match align {
            Align::Left => key.x + 8.0,
            Align::Right => key.x + key.w - tw - 8.0,
            Align::Center => key.x + (key.w - tw) / 2.0,
        };
        let y = match valign {
            VAlign::Top => key.y + 3.0,
            VAlign::Bottom => key.y + key.h - th - 4.0,
            VAlign::Center => key.y + (key.h - th) / 2.0,
        };
        let dst = SDL_FRect { x, y, w: tw, h: th };
        unsafe {
            (sdl().render_texture)(self.renderer, tex.0, std::ptr::null(), &dst);
        }
    }

    /// Re-assert the always-on-top hint AND directly restack; compositors/WMs
    /// drop topmost when other windows raise (or ignore the state after
    /// repeated hide/show cycles), so the main loop calls this periodically
    /// (same reason adusk.py has `_reassert_topmost`).
    pub fn assert_topmost(&self) {
        unsafe {
            let s = sdl();
            (s.set_window_always_on_top)(self.window, true);
            sdl3::raise_window(self.window);
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
