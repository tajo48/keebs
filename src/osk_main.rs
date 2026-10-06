//! OSK application loop — Rust port of `adusk.main` (lean v1: no X11/Win32
//! no-activate calls, no open animation, no click-through or live skin
//! reload; those ride on the tray/settings phases).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::manager;
use crate::osk_state::OskState;
use crate::render::{FrameLimiter, Screen};
use crate::sdl3::{self, sdl};
use crate::stop::StopFlag;
use crate::vkb::{Dispatcher, KEY_REPEAT_DELAY, KEY_REPEAT_INTERVAL, VirtualKeyboard};

const ACTIVE_FPS: f32 = 120.0;
const IDLE_FPS: f32 = 15.0;
const IDLE_GRACE: f32 = 0.4;

pub fn run(stop: Arc<StopFlag>) {
    // Force X11/XWayland like the Python app's effective environment:
    // SDL_SetWindowAlwaysOnTop and the X11 no-focus WM hints only work there
    // (on native Wayland both are no-ops — confirmed by our own backend log).
    // A real env var, not an SDL hint: the hint didn't stick in practice.
    // Respected only when the user hasn't chosen a driver explicitly.
    if std::env::var_os("SDL_VIDEODRIVER").is_none() {
        // SAFETY: single-threaded here — no other thread has been spawned.
        unsafe {
            std::env::set_var("SDL_VIDEODRIVER", "x11");
        }
    }

    let s = sdl();
    let state = Arc::new(OskState::new());
    let (w, h) = (1286.0_f32, 369.0_f32);
    state.width.store(w as usize, Ordering::SeqCst);
    state.height.store(h as usize, Ordering::SeqCst);

    unsafe {
        let hint = cstring_hint();
        let _ = (s.set_hint)(hint.as_ptr(), b"1\0".as_ptr() as *const _);
        if !(s.init)(sdl3::SDL_INIT_VIDEO | sdl3::SDL_INIT_EVENTS) {
            eprintln!("osk: SDL_Init failed: {}", s.get_error_string());
            return;
        }
    }

    // Controller input thread (owns the device, lizard OFF).
    {
        let st = Arc::clone(&state);
        let sp = Arc::clone(&stop);
        std::thread::spawn(move || manager::input_thread(st, sp));
    }

    let mut screen = Screen::new(&state);
    let mut vkb = VirtualKeyboard::new(crate::vkb::default_layout(), w, h);
    state.set_grid_dims(vkb.keys.iter().map(|r| r.len()).collect());

    let kb = match crate::uinput::Keyboard::new() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("osk: uinput init failed ({e}); dispatch disabled");
            return;
        }
    };

    let mut was_visible = false;
    let mut current_fps = ACTIVE_FPS;
    let mut last_active = Instant::now();
    let mut limiter = FrameLimiter::new(ACTIVE_FPS);
    let mut next_topmost = Instant::now();
    // The first motion event after open just records the anchor (see adusk.py).
    let mut mouse_anchor: Option<(i32, i32)> = None;
    let mut mouse_repeat_at: Option<Instant> = None;

    while !stop.is_set() && !state.should_close_app() {
        let mut activity = false;

        // --- SDL events -----------------------------------------------------
        unsafe {
            let mut ev = sdl3::SDL_Event::default();
            while (s.poll_event)(&mut ev) {
                let et = ev.common.type_;
                if et == sdl3::SDL_EVENT_QUIT {
                    state.close_app.store(true, Ordering::SeqCst);
                    break;
                }
                if et == sdl3::SDL_EVENT_WINDOW_RESIZED {
                    let (nw, nh) = (ev.window.data1.max(1), ev.window.data2.max(1));
                    state.width.store(nw as usize, Ordering::SeqCst);
                    state.height.store(nh as usize, Ordering::SeqCst);
                    vkb.update_dimensions(nw as f32, nh as f32);
                    activity = true;
                } else if et == sdl3::SDL_EVENT_MOUSE_MOTION && state.is_visible() {
                    let mpos = (ev.motion.x as i32, ev.motion.y as i32);
                    if let Some(anchor) = mouse_anchor {
                        if mpos != anchor {
                            if let Some((r, c)) = vkb.find_key(mpos.0 as f32, mpos.1 as f32) {
                                state.set_cursor(r, c);
                            }
                            activity = true;
                        }
                    }
                    mouse_anchor = Some(mpos);
                    if ev.motion.state & sdl3::SDL_BUTTON_LMASK == 0 {
                        state.set_mouse_press_cell(None);
                        mouse_repeat_at = None;
                    }
                } else if et == sdl3::SDL_EVENT_MOUSE_BUTTON_DOWN && state.is_visible() {
                    let btn = ev.button.button;
                    mouse_anchor = Some((ev.button.x as i32, ev.button.y as i32));
                    if btn == sdl3::SDL_BUTTON_LEFT {
                        if let Some((r, c)) = vkb.find_key(ev.button.x as f32, ev.button.y as f32) {
                            state.set_cursor(r, c);
                            state.set_mouse_press_cell(Some((r, c)));
                            state.queue_key_press(r, c, false);
                            mouse_repeat_at =
                                Some(Instant::now() + Duration::from_secs_f32(KEY_REPEAT_DELAY));
                        }
                        activity = true;
                    } else if btn == sdl3::SDL_BUTTON_RIGHT {
                        Dispatcher {
                            kb: &kb,
                            state: &state,
                        }
                        .toggle_shift();
                        activity = true;
                    } else if btn == sdl3::SDL_BUTTON_X1 {
                        kb.tap_code(crate::uinput::keys::KEY_BACKSPACE);
                        activity = true;
                    } else if btn == sdl3::SDL_BUTTON_X2 {
                        kb.tap_code(crate::uinput::keys::KEY_SPACE);
                        activity = true;
                    }
                } else if et == sdl3::SDL_EVENT_MOUSE_BUTTON_UP
                    && ev.button.button == sdl3::SDL_BUTTON_LEFT
                {
                    state.set_mouse_press_cell(None);
                    mouse_repeat_at = None;
                    activity = true;
                }
            }
        }

        let cur_visible = state.is_visible();
        if cur_visible != was_visible {
            if cur_visible {
                screen.show();
                mouse_anchor = None; // re-prime: opening fires a spurious motion
            } else {
                Dispatcher {
                    kb: &kb,
                    state: &state,
                }
                .release_shift();
                screen.hide();
                mouse_repeat_at = None;
            }
            was_visible = cur_visible;
            activity = true;
        }

        if cur_visible {
            // Compositors drop topmost when other windows raise — re-assert.
            if Instant::now() >= next_topmost {
                screen.assert_topmost();
                next_topmost = Instant::now() + Duration::from_millis(500);
            }
            // DPAD / left-stick cursor steps.
            let dpad_steps = state.drain_dpad_queue();
            for (dir, _haptic) in &dpad_steps {
                vkb.step_cursor(&state, *dir, *_haptic);
            }
            activity |= !dpad_steps.is_empty();

            // Pad clicks -> find key -> dispatch (repeats only over
            // repeatable keys).
            for click in state.drain_click_queue() {
                let (x, y) = state.frac_to_abs(click.frac);
                if let Some((r, c)) = vkb.find_key(x, y) {
                    if click.is_repeat && !vkb.is_repeatable(r, c) {
                        continue;
                    }
                    let kd = &vkb.keys[r][c];
                    println!("osk: dispatch pad-click '{}' ({r},{c})", kd.label);
                    Dispatcher {
                        kb: &kb,
                        state: &state,
                    }
                    .dispatch(kd);
                }
            }

            // Mouse hold-to-repeat over repeatable keys.
            if let (Some(cell), Some(at)) = (state.get_mouse_press_cell(), mouse_repeat_at) {
                activity = true;
                if Instant::now() >= at {
                    let (pr, pc) = cell;
                    if vkb.is_repeatable(pr, pc) {
                        state.queue_key_press(pr, pc, true);
                        mouse_repeat_at =
                            Some(Instant::now() + Duration::from_secs_f32(KEY_REPEAT_INTERVAL));
                    } else {
                        mouse_repeat_at = None;
                    }
                }
            }

            // Queued key presses (A button, mouse clicks).
            for (r, c, is_repeat) in state.drain_key_press_queue() {
                if r < vkb.rows() && c < vkb.row_len(r) {
                    if is_repeat && !vkb.is_repeatable(r, c) {
                        continue;
                    }
                    let kd = &vkb.keys[r][c];
                    println!("osk: dispatch key '{}' ({r},{c})", kd.label);
                    Dispatcher {
                        kb: &kb,
                        state: &state,
                    }
                    .dispatch(kd);
                }
            }

            // Clicks while hidden would pile up — the manager clears them by
            // never queueing when hidden is not enforced, so drain here too.
            state.drain_click_queue();

            screen.render(&state, &vkb);
        } else {
            // Drain queues while hidden so they don't pile up.
            state.drain_key_press_queue();
            state.drain_dpad_queue();
            state.drain_click_queue();
        }

        // Adaptive FPS: fast while active, slow when idle (grace period).
        if activity {
            last_active = Instant::now();
        }
        let desired_fps = if last_active.elapsed().as_secs_f32() < IDLE_GRACE {
            ACTIVE_FPS
        } else {
            IDLE_FPS
        };
        if desired_fps != current_fps {
            limiter.set_fps(desired_fps);
            current_fps = desired_fps;
        }
        limiter.delay();
    }

    Dispatcher {
        kb: &kb,
        state: &state,
    }
    .release_shift();
    state.close_app.store(true, Ordering::SeqCst);
    stop.signal(); // release the controller thread
    println!("osk: bye");
}

fn cstring_hint() -> std::ffi::CString {
    std::ffi::CString::new("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS").unwrap()
}
