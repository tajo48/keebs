//! Shared state between the OSK render loop (main thread) and the controller
//! input thread. Port of the relevant `adusk.state` module surface: cursor,
//! shift/caps latch, highlights, queues, visibility, pad pointers.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum InputState {
    Inactive = 0,
    Hover = 1,
    Click = 2,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Pointer {
    pub state: InputState,
    /// Fractional coords in OSK-window space (0..1, 0..1).
    pub frac: (f32, f32),
}

impl Pointer {
    pub const INACTIVE: Pointer = Pointer {
        state: InputState::Inactive,
        frac: (0.0, 0.0),
    };

    pub fn in_box(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        let (px, py) = self.frac;
        px >= x && py >= y && px <= x + w && py <= y + h
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    pub fn name(self) -> &'static str {
        match self {
            Dir::Up => "UP",
            Dir::Down => "DOWN",
            Dir::Left => "LEFT",
            Dir::Right => "RIGHT",
        }
    }
}

/// A queued "click at this fractional coord" (`is_repeat` = hold-to-repeat
/// hit, honored only over repeatable keys).
#[derive(Clone, Copy)]
pub struct ClickEvent {
    pub frac: (f32, f32),
    pub is_repeat: bool,
}

pub struct OskState {
    pub visible: AtomicBool,
    pub close_app: AtomicBool,
    pub width: AtomicUsize,
    pub height: AtomicUsize,

    cursor: Mutex<(usize, usize)>,
    grid_dims: Mutex<Vec<usize>>,
    shift_held: AtomicBool,
    shift_latched: AtomicBool,
    emoji_open: AtomicBool,
    /// Cached Caps Lock state — the live X11 query is a synchronous
    /// round-trip and must never run per render frame (it stalled typing).
    /// Refreshed on a slow tick and whenever CapsLock is injected.
    caps_on: AtomicBool,
    highlighted: Mutex<HashSet<u16>>,
    lpad_touched: AtomicBool,
    rpad_touched: AtomicBool,
    mouse_press_cell: Mutex<Option<(usize, usize)>>,
    key_press_queue: Mutex<Vec<(usize, usize, bool)>>,
    dpad_queue: Mutex<Vec<(Dir, bool)>>,
    click_queue: Mutex<Vec<ClickEvent>>,
    pointers: Mutex<[Pointer; 2]>,
    haptic_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl OskState {
    pub fn new() -> Self {
        Self {
            visible: AtomicBool::new(true),
            close_app: AtomicBool::new(false),
            width: AtomicUsize::new(1286),
            height: AtomicUsize::new(369),
            cursor: Mutex::new((2, 1)),
            grid_dims: Mutex::new(Vec::new()),
            shift_held: AtomicBool::new(false),
            shift_latched: AtomicBool::new(false),
            emoji_open: AtomicBool::new(false),
            caps_on: AtomicBool::new(false),
            highlighted: Mutex::new(HashSet::new()),
            lpad_touched: AtomicBool::new(false),
            rpad_touched: AtomicBool::new(false),
            mouse_press_cell: Mutex::new(None),
            key_press_queue: Mutex::new(Vec::new()),
            dpad_queue: Mutex::new(Vec::new()),
            click_queue: Mutex::new(Vec::new()),
            pointers: Mutex::new([Pointer::INACTIVE, Pointer::INACTIVE]),
            haptic_hook: Mutex::new(None),
        }
    }

    // -- simple flags -------------------------------------------------------

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }
    pub fn show(&self) {
        self.visible.store(true, Ordering::SeqCst);
    }
    /// Hide the OSK window (the Python `state.close()` — but in this single
    /// process that means "hide", not "quit"; Ctrl-C quits the app).
    pub fn close(&self) {
        self.visible.store(false, Ordering::SeqCst);
    }
    pub fn should_close_app(&self) -> bool {
        self.close_app.load(Ordering::SeqCst)
    }
    pub fn is_shift_held(&self) -> bool {
        self.shift_held.load(Ordering::SeqCst)
    }
    pub fn set_shift_held(&self, v: bool) {
        self.shift_held.store(v, Ordering::SeqCst);
    }
    pub fn is_shift_latched(&self) -> bool {
        self.shift_latched.load(Ordering::SeqCst)
    }
    pub fn set_shift_latched(&self, v: bool) {
        self.shift_latched.store(v, Ordering::SeqCst);
    }
    /// Cached Caps Lock state (adusk.state `is_caps_on`): physical and
    /// injected CapsLock stay in sync via periodic refresh — see
    /// `refresh_caps` in the main loop. Reads must stay cheap: this runs
    /// every render frame.
    pub fn is_caps_on(&self) -> bool {
        self.caps_on.load(Ordering::SeqCst)
    }
    /// Re-read Caps Lock from the X server's modifier mask. Called on the
    /// slow tick (~4 Hz) and right after a CapsLock injection, never per
    /// frame — the X query is a blocking round-trip.
    pub fn refresh_caps(&self) {
        self.caps_on
            .store(crate::sdl3::x11_caps_on(), Ordering::SeqCst);
    }
    pub fn is_emoji_open(&self) -> bool {
        self.emoji_open.load(Ordering::SeqCst)
    }
    pub fn set_emoji_open(&self, v: bool) {
        self.emoji_open.store(v, Ordering::SeqCst);
    }
    pub fn is_lpad_touched(&self) -> bool {
        self.lpad_touched.load(Ordering::SeqCst)
    }
    pub fn is_rpad_touched(&self) -> bool {
        self.rpad_touched.load(Ordering::SeqCst)
    }
    pub fn set_pad_touched(&self, l: bool, r: bool) {
        self.lpad_touched.store(l, Ordering::SeqCst);
        self.rpad_touched.store(r, Ordering::SeqCst);
    }

    // -- highlights ---------------------------------------------------------

    pub fn set_highlighted(&self, set: HashSet<u16>) {
        *self.highlighted.lock().unwrap() = set;
    }

    pub fn get_highlighted(&self) -> HashSet<u16> {
        self.highlighted.lock().unwrap().clone()
    }

    // -- cursor / grid ------------------------------------------------------

    pub fn set_grid_dims(&self, dims: Vec<usize>) {
        *self.grid_dims.lock().unwrap() = dims;
    }

    pub fn get_cursor(&self) -> (usize, usize) {
        *self.cursor.lock().unwrap()
    }

    pub fn set_cursor(&self, row: usize, col: usize) {
        *self.cursor.lock().unwrap() = (row, col);
    }

    pub fn get_mouse_press_cell(&self) -> Option<(usize, usize)> {
        *self.mouse_press_cell.lock().unwrap()
    }

    pub fn set_mouse_press_cell(&self, cell: Option<(usize, usize)>) {
        *self.mouse_press_cell.lock().unwrap() = cell;
    }

    // -- queues -------------------------------------------------------------

    pub fn queue_key_press(&self, row: usize, col: usize, is_repeat: bool) {
        self.key_press_queue
            .lock()
            .unwrap()
            .push((row, col, is_repeat));
    }

    pub fn drain_key_press_queue(&self) -> Vec<(usize, usize, bool)> {
        std::mem::take(&mut *self.key_press_queue.lock().unwrap())
    }

    pub fn queue_dpad(&self, dir: Dir, haptic: bool) {
        self.dpad_queue.lock().unwrap().push((dir, haptic));
    }

    pub fn drain_dpad_queue(&self) -> Vec<(Dir, bool)> {
        std::mem::take(&mut *self.dpad_queue.lock().unwrap())
    }

    pub fn queue_click(&self, frac: (f32, f32), is_repeat: bool) {
        self.click_queue
            .lock()
            .unwrap()
            .push(ClickEvent { frac, is_repeat });
    }

    pub fn drain_click_queue(&self) -> Vec<ClickEvent> {
        std::mem::take(&mut *self.click_queue.lock().unwrap())
    }

    // -- haptics ------------------------------------------------------------

    /// The Python `state.set_haptic_tick` equivalent: the input thread
    /// installs a closure that buzzes the live controller; the render loop
    /// fires it per dispatched key press.
    pub fn set_haptic_tick(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.haptic_hook.lock().unwrap() = Some(f);
    }

    pub fn haptic_tick(&self) {
        if let Some(f) = self.haptic_hook.lock().unwrap().as_ref() {
            f();
        }
    }

    // -- pointers -----------------------------------------------------------

    pub fn set_pointers(&self, left: Pointer, right: Pointer) {
        *self.pointers.lock().unwrap() = [left, right];
    }

    pub fn get_pointers(&self) -> [Pointer; 2] {
        *self.pointers.lock().unwrap()
    }

    // -- coords -------------------------------------------------------------

    pub fn abs_dims(&self) -> (f32, f32) {
        (
            self.width.load(Ordering::SeqCst) as f32,
            self.height.load(Ordering::SeqCst) as f32,
        )
    }

    /// Absolute pixel coords from a fractional pointer position.
    pub fn frac_to_abs(&self, frac: (f32, f32)) -> (f32, f32) {
        let (w, h) = self.abs_dims();
        (frac.0 * w, frac.1 * h)
    }

    /// Fractional coords from absolute pixels (pad raw values are mapped to
    /// absolute pixels first — see manager::adjust_raw).
    pub fn abs_to_frac(&self, x: f32, y: f32) -> (f32, f32) {
        let (w, h) = self.abs_dims();
        (x / w, y / h)
    }
}
