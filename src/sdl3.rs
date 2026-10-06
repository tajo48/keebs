//! Hand-rolled Rust binding for the subset of SDL3 + SDL3_ttf this project
//! uses — a direct port of the Python `sdl3w` ctypes shim (`linux/sdl3w`).
//!
//! Loads the system `libSDL3.so.0` / `libSDL3_ttf.so.0` at runtime via
//! dlopen (same sonames and fallbacks as sdl3w's `_loader.py`; SDL3_ttf is
//! loaded after SDL3 so its NEEDED dependency is already resolved).
//!
//! SDL3 API notes kept from sdl3w:
//!   * window flags are Uint64, most functions return bool (true = success)
//!   * SDL_CreateWindow has no x/y args — position via SDL_SetWindowPosition
//!   * SDL_RenderTexture takes float rects; fills take SDL_FRect too
//!   * TTF_RenderText_Blended takes (font, text, length, SDL_Color by value)

#![allow(dead_code)]

use libloading::Library;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::OnceLock;

pub type SdlWindow = *mut c_void;
pub type SdlRenderer = *mut c_void;
pub type SdlTexture = *mut c_void;
pub type TtfFont = *mut c_void;

// ---------------------------------------------------------------------------
// Constants (mirror sdl3w).
// ---------------------------------------------------------------------------

pub const SDL_INIT_VIDEO: u32 = 0x0000_0020;
pub const SDL_INIT_EVENTS: u32 = 0x0000_4000;

pub const SDL_WINDOW_HIDDEN: u64 = 0x0000_0000_0000_0008;
pub const SDL_WINDOW_BORDERLESS: u64 = 0x0000_0000_0000_0010;
pub const SDL_WINDOW_ALWAYS_ON_TOP: u64 = 0x0000_0000_0000_0100;
pub const SDL_WINDOW_UTILITY: u64 = 0x0000_0000_0002_0000;
pub const SDL_WINDOW_NOT_FOCUSABLE: u64 = 0x0000_0000_0008_0000;
pub const SDL_WINDOW_TRANSPARENT: u64 = 0x0000_0000_4000_0000;

pub const SDL_WINDOWPOS_CENTERED: i32 = 0x2FFF_0000;

pub const SDL_BLENDMODE_BLEND: u32 = 0x0000_0001;

pub const SDL_EVENT_QUIT: u32 = 0x100;
pub const SDL_EVENT_WINDOW_RESIZED: u32 = 0x206;
pub const SDL_EVENT_MOUSE_MOTION: u32 = 0x400;
pub const SDL_EVENT_MOUSE_BUTTON_DOWN: u32 = 0x401;
pub const SDL_EVENT_MOUSE_BUTTON_UP: u32 = 0x402;

pub const SDL_BUTTON_LEFT: u8 = 1;
pub const SDL_BUTTON_RIGHT: u8 = 3;
pub const SDL_BUTTON_X1: u8 = 4;
pub const SDL_BUTTON_X2: u8 = 5;
pub const SDL_BUTTON_LMASK: u32 = 1;

// ---------------------------------------------------------------------------
// Structs (exact C layouts, mirroring sdl3w's ctypes declarations).
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SDL_Rect {
    pub x: c_int,
    pub y: c_int,
    pub w: c_int,
    pub h: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SDL_FRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SDL_Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Only `w`/`h` are read (they precede any pointer field, like sdl3w).
#[repr(C)]
pub struct SDL_Surface {
    flags: u32,
    format: c_int,
    pub w: c_int,
    pub h: c_int,
    pitch: c_int,
    pixels: *mut c_void,
    refcount: c_int,
    reserved: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SDL_CommonEvent {
    pub type_: u32,
    reserved: u32,
    timestamp: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SDL_WindowEvent {
    pub type_: u32,
    reserved: u32,
    timestamp: u64,
    window_id: u32,
    pub data1: i32,
    pub data2: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SDL_MouseMotionEvent {
    pub type_: u32,
    reserved: u32,
    timestamp: u64,
    window_id: u32,
    which: u32,
    pub state: u32,
    pub x: f32,
    pub y: f32,
    xrel: f32,
    yrel: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SDL_MouseButtonEvent {
    pub type_: u32,
    reserved: u32,
    timestamp: u64,
    window_id: u32,
    which: u32,
    pub button: u8,
    down: bool,
    clicks: u8,
    padding: u8,
    pub x: f32,
    pub y: f32,
}

#[repr(C)]
pub union SDL_Event {
    pub common: SDL_CommonEvent,
    pub window: SDL_WindowEvent,
    pub motion: SDL_MouseMotionEvent,
    pub button: SDL_MouseButtonEvent,
    pub padding: [u8; 128],
}

impl Default for SDL_Event {
    fn default() -> Self {
        SDL_Event {
            padding: [0u8; 128],
        }
    }
}

// ---------------------------------------------------------------------------
// Loader.
// ---------------------------------------------------------------------------

type FnBool1u32 = unsafe extern "C" fn(u32) -> bool;
type FnVoid1u32 = unsafe extern "C" fn(u32);
type FnBool2CharPtr = unsafe extern "C" fn(*const c_char, *const c_char) -> bool;
type FnCharPtr = unsafe extern "C" fn() -> *const c_char;
type FnU32 = unsafe extern "C" fn() -> u32;
type FnBoolU32Rect = unsafe extern "C" fn(u32, *mut SDL_Rect) -> bool;
type FnWindow = unsafe extern "C" fn(*const c_char, c_int, c_int, u64) -> SdlWindow;
type FnVoidPtr = unsafe extern "C" fn(SdlWindow);
type FnBoolWin2i = unsafe extern "C" fn(SdlWindow, c_int, c_int) -> bool;
type FnRenderer = unsafe extern "C" fn(SdlWindow, *const c_char) -> SdlRenderer;
type FnVoidRenderer = unsafe extern "C" fn(SdlRenderer);
type FnBoolR4u8 = unsafe extern "C" fn(SdlRenderer, u8, u8, u8, u8) -> bool;
type FnBoolRU32 = unsafe extern "C" fn(SdlRenderer, u32) -> bool;
type FnBoolR = unsafe extern "C" fn(SdlRenderer) -> bool;
type FnBoolRFRect = unsafe extern "C" fn(SdlRenderer, *const SDL_FRect) -> bool;
type FnBoolRTex =
    unsafe extern "C" fn(SdlRenderer, SdlTexture, *const SDL_FRect, *const SDL_FRect) -> bool;
type FnBoolR4f = unsafe extern "C" fn(SdlRenderer, f32, f32, f32, f32) -> bool;
type FnTexture = unsafe extern "C" fn(SdlRenderer, *mut SDL_Surface) -> SdlTexture;
type FnVoidTexture = unsafe extern "C" fn(SdlTexture);
type FnBoolTu8 = unsafe extern "C" fn(SdlTexture, u8) -> bool;
type FnSurface = unsafe extern "C" fn(TtfFont, *const c_char, usize, SDL_Color) -> *mut SDL_Surface;
type FnFont = unsafe extern "C" fn(*const c_char, f32) -> TtfFont;
type FnVoidFont = unsafe extern "C" fn(TtfFont);
type FnVoidSurface = unsafe extern "C" fn(*mut SDL_Surface);
type FnBoolTtf = unsafe extern "C" fn() -> bool;
type FnBoolEvent = unsafe extern "C" fn(*mut SDL_Event) -> bool;

pub struct Sdl {
    _core: Library,
    _ttf: Library,
    pub init: FnBool1u32,
    pub set_hint: FnBool2CharPtr,
    pub get_error: FnCharPtr,
    pub get_primary_display: FnU32,
    pub get_display_usable_bounds: FnBoolU32Rect,
    pub create_window: FnWindow,
    pub destroy_window: FnVoidPtr,
    pub set_window_position: FnBoolWin2i,
    pub show_window: FnVoidPtr,
    pub hide_window: FnVoidPtr,
    pub create_renderer: FnRenderer,
    pub destroy_renderer: FnVoidRenderer,
    pub set_render_draw_color: FnBoolR4u8,
    pub set_render_draw_blend_mode: FnBoolRU32,
    pub render_clear: FnBoolR,
    pub render_fill_rect: FnBoolRFRect,
    pub render_texture: FnBoolRTex,
    pub render_line: FnBoolR4f,
    pub render_present: FnBoolR,
    pub create_texture_from_surface: FnTexture,
    pub destroy_texture: FnVoidTexture,
    pub set_texture_alpha_mod: FnBoolTu8,
    pub poll_event: FnBoolEvent,
    pub ttf_init: FnBoolTtf,
    pub ttf_open_font: FnFont,
    pub ttf_close_font: FnVoidFont,
    pub ttf_render_text_blended: FnSurface,
    pub destroy_surface: FnVoidSurface,
}

unsafe fn bind<'a, T: Copy>(lib: &'a Library, name: &[u8]) -> T {
    match unsafe { lib.get::<T>(name) } {
        Ok(sym) => *sym,
        Err(e) => panic!(
            "sdl3: missing symbol {:?}: {e}",
            String::from_utf8_lossy(name)
        ),
    }
}

impl Sdl {
    fn load() -> Result<Self, String> {
        // Same sonames as sdl3w: most-specific first, resolved by ld.so.
        const CORE: [&[u8]; 2] = [b"libSDL3.so.0\0", b"libSDL3.so\0"];
        const TTF: [&[u8]; 2] = [b"libSDL3_ttf.so.0\0", b"libSDL3_ttf.so\0"];

        let core = load_first(&CORE).ok_or("could not load libSDL3 (pacman -S sdl3)?")?;
        let ttf = load_first(&TTF).ok_or("could not load libSDL3_ttf (pacman -S sdl3_ttf)?")?;

        // SAFETY: symbol names/signatures match SDL 3.4.x / SDL3_ttf 3.2.x as
        // verified by the Python sdl3w shim on the same libs.
        Ok(unsafe {
            Self {
                init: bind(&core, b"SDL_Init\0"),
                set_hint: bind(&core, b"SDL_SetHint\0"),
                get_error: bind(&core, b"SDL_GetError\0"),
                get_primary_display: bind(&core, b"SDL_GetPrimaryDisplay\0"),
                get_display_usable_bounds: bind(&core, b"SDL_GetDisplayUsableBounds\0"),
                create_window: bind(&core, b"SDL_CreateWindow\0"),
                destroy_window: bind(&core, b"SDL_DestroyWindow\0"),
                set_window_position: bind(&core, b"SDL_SetWindowPosition\0"),
                show_window: bind(&core, b"SDL_ShowWindow\0"),
                hide_window: bind(&core, b"SDL_HideWindow\0"),
                create_renderer: bind(&core, b"SDL_CreateRenderer\0"),
                destroy_renderer: bind(&core, b"SDL_DestroyRenderer\0"),
                set_render_draw_color: bind(&core, b"SDL_SetRenderDrawColor\0"),
                set_render_draw_blend_mode: bind(&core, b"SDL_SetRenderDrawBlendMode\0"),
                render_clear: bind(&core, b"SDL_RenderClear\0"),
                render_fill_rect: bind(&core, b"SDL_RenderFillRect\0"),
                render_texture: bind(&core, b"SDL_RenderTexture\0"),
                render_line: bind(&core, b"SDL_RenderLine\0"),
                render_present: bind(&core, b"SDL_RenderPresent\0"),
                create_texture_from_surface: bind(&core, b"SDL_CreateTextureFromSurface\0"),
                destroy_texture: bind(&core, b"SDL_DestroyTexture\0"),
                set_texture_alpha_mod: bind(&core, b"SDL_SetTextureAlphaMod\0"),
                poll_event: bind(&core, b"SDL_PollEvent\0"),
                ttf_init: bind(&ttf, b"TTF_Init\0"),
                ttf_open_font: bind(&ttf, b"TTF_OpenFont\0"),
                ttf_close_font: bind(&ttf, b"TTF_CloseFont\0"),
                ttf_render_text_blended: bind(&ttf, b"TTF_RenderText_Blended\0"),
                destroy_surface: bind(&ttf, b"SDL_DestroySurface\0"),
                _core: core,
                _ttf: ttf,
            }
        })
    }

    pub fn get_error_string(&self) -> String {
        unsafe {
            let p = (self.get_error)();
            if p.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }
}

fn load_first(names: &[&[u8]]) -> Option<Library> {
    // Extra search dirs for nix systems, where SDL3 lives in the store and
    // is only on LD_LIBRARY_PATH inside `nix develop` (same reason the
    // Python flake sets LD_LIBRARY_PATH).
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(format!("{home}/.nix-profile/lib"));
        let user = home.split('/').next_back().unwrap_or("");
        dirs.push(format!("/etc/profiles/per-user/{user}/lib"));
    }
    dirs.push("/run/current-system/sw/lib".to_string());

    for n in names {
        // Bare soname first: resolved by ld.so from LD_LIBRARY_PATH etc.
        let Ok(c) = std::ffi::CStr::from_bytes_with_nul(n) else {
            continue;
        };
        if let Ok(lib) = unsafe { Library::new(c.to_string_lossy().as_ref()) } {
            return Some(lib);
        }
        for dir in &dirs {
            let path = format!(
                "{dir}/{}",
                String::from_utf8_lossy(n).trim_end_matches('\0')
            );
            if let Ok(lib) = unsafe { Library::new(&path) } {
                eprintln!("sdl3: loaded {} from nix fallback path", path);
                return Some(lib);
            }
        }
    }
    None
}

static SDL: OnceLock<Sdl> = OnceLock::new();

pub fn sdl() -> &'static Sdl {
    SDL.get_or_init(|| Sdl::load().unwrap_or_else(|e| panic!("sdl3: {e}")))
}

/// Convenience: render UTF-8 text to a blended surface, upload as a texture,
/// destroy the surface. Returns (texture, w, h).
pub fn render_text_texture(
    renderer: SdlRenderer,
    font: TtfFont,
    text: &str,
    color: SDL_Color,
) -> Option<(SdlTexture, i32, i32)> {
    if text.is_empty() {
        return None;
    }
    let s = sdl();
    let ctext = CString::new(text).ok()?;
    unsafe {
        let surface = (s.ttf_render_text_blended)(font, ctext.as_ptr(), 0, color);
        if surface.is_null() {
            return None;
        }
        let (w, h) = ((*surface).w, (*surface).h);
        let tex = (s.create_texture_from_surface)(renderer, surface);
        (s.destroy_surface)(surface);
        if tex.is_null() {
            None
        } else {
            Some((tex, w, h))
        }
    }
}
