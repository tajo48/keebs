//! Wayland text-input-aware auto-show for the OSK (plan.md phases 0–2).
//!
//! A dedicated thread owns a Wayland connection (the input-method-v2
//! connection, not the render surface) and translates
//! `zwp_input_method_v2` activate/deactivate events into visibility
//! decisions via `OskState::decide`. SDL3 stays a pure renderer: it knows
//! nothing about Wayland protocols.
//!
//! Socket probing: most compositors (KWin, sway, Hyprland, wlroots) expose
//! `zwp_input_method_manager_v2` on the ambient `WAYLAND_DISPLAY`, but some
//! expose the input-method socket as a sibling (e.g. `wayland-1`). Probe the
//! ambient display first, then any other `wayland-*` sockets in
//! `XDG_RUNTIME_DIR`, and log which one worked. No usable Wayland display or
//! no IM global → the thread exits and the OSK keeps its manual-only
//! behavior (X11 and plain GNOME land here by design).

use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::wayland_im_protocol::{zwp_input_method_manager_v2, zwp_input_method_v2};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

use crate::diag;
use crate::osk_state::{OskEvent, OskState};
use crate::stop::StopFlag;

/// Focus flaps happen during window switches — don't hide on the first
/// deactivate blip (plan.md state machine).
const HIDE_GRACE: Duration = Duration::from_millis(250);
/// Cap on the socket poll wait so the thread stays stop-responsive even
/// when the compositor is silent.
const POLL_CAP_MS: i32 = 200;

const IM_MANAGER_IFACE: &str = "zwp_input_method_manager_v2";

/// One advertised global from the registry walk.
struct KnownGlobal {
    name: u32,
    interface: String,
    version: u32,
}

/// Dispatch state for the IM connection. Lives on the wl thread only.
struct ImState {
    osk: Arc<OskState>,
    stop: Arc<StopFlag>,
    /// Deadline for the post-deactivate grace-period hide.
    hide_at: Option<Instant>,
    /// Globals seen in the last registry roundtrip.
    globals: Vec<KnownGlobal>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ImState {
    fn event(
        st: &mut Self,
        _: &wl_registry::WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match ev {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => st.globals.push(KnownGlobal {
                name,
                interface,
                version,
            }),
            wl_registry::Event::GlobalRemove { name } => {
                st.globals.retain(|g| g.name != name);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ImState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// The manager interface has no events.
impl Dispatch<zwp_input_method_manager_v2::ZwpInputMethodManagerV2, ()> for ImState {
    fn event(
        _: &mut Self,
        _: &zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        _: zwp_input_method_manager_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for ImState {
    fn event(
        st: &mut Self,
        _: &zwp_input_method_v2::ZwpInputMethodV2,
        ev: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_input_method_v2::Event;
        match ev {
            Event::Activate => {
                st.hide_at = None;
                st.osk.decide(OskEvent::ImActivate);
            }
            Event::Deactivate => {
                st.hide_at = Some(Instant::now() + HIDE_GRACE);
            }
            Event::Unavailable => {
                // Our IM slot was taken (e.g. another IM grabbed the seat) —
                // treat as an immediate deactivate.
                st.hide_at = None;
                st.osk.decide(OskEvent::ImDeactivate);
            }
            _ => {}
        }
    }
}

/// Spawn the IM monitor thread, or `None` when there is no Wayland display
/// to talk to (X11 session → caller just doesn't get auto-show).
pub fn spawn(osk: Arc<OskState>, stop: Arc<StopFlag>) -> Option<JoinHandle<()>> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && sibling_sockets().is_empty() {
        return None;
    }
    let state = ImState {
        osk,
        stop,
        hide_at: None,
        globals: Vec::new(),
    };
    Some(std::thread::spawn(move || run(state)))
}

/// Sibling `wayland-*` socket names in `XDG_RUNTIME_DIR` (the input-method
/// socket is sometimes published next to the regular one). Only proper
/// socket names match: `wayland-` followed by digits, so lock files like
/// `wayland-0.lock` are skipped.
fn sibling_sockets() -> Vec<OsString> {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Vec::new();
    };
    let is_socket_name = |n: &OsString| -> bool {
        let Some(suffix) = n.as_encoded_bytes().strip_prefix(b"wayland-") else {
            return false;
        };
        !suffix.is_empty() && suffix.iter().all(u8::is_ascii_digit)
    };
    let mut out: Vec<OsString> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name())
        .filter(|n| is_socket_name(n))
        .collect();
    out.sort();
    out
}

/// Bind seat + input-method manager on `conn`. Returns the event queue that
/// owns the dispatching, or `None` if the IM manager global is absent (the
/// connection is dropped by the caller then).
fn setup_im(conn: &Connection, state: &mut ImState) -> Option<EventQueue<ImState>> {
    state.globals.clear();
    let queue = conn.new_event_queue::<ImState>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut queue = queue;
    queue.roundtrip(state).ok()?;
    let seat_g = state.globals.iter().find(|g| g.interface == "wl_seat")?;
    let im_g = state
        .globals
        .iter()
        .find(|g| g.interface == IM_MANAGER_IFACE)?;
    // Seat v1 is enough: IM v2 only needs it for the grab handshake.
    let seat: wl_seat::WlSeat = registry.bind(seat_g.name, 1, &qh, ());
    let manager: zwp_input_method_manager_v2::ZwpInputMethodManagerV2 =
        registry.bind(im_g.name, im_g.version, &qh, ());
    manager.get_input_method(&seat, &qh, ());
    Some(queue)
}

fn run(mut state: ImState) {
    // 1) Ambient display (WAYLAND_DISPLAY / WAYLAND_SOCKET), 2) siblings.
    let mut found: Option<(Connection, EventQueue<ImState>, String)> = None;
    match Connection::connect_to_env() {
        Ok(conn) => {
            if let Some(queue) = setup_im(&conn, &mut state) {
                found = Some((conn, queue, "WAYLAND_DISPLAY".into()));
            } else if state.stop.is_set() {
                return;
            } else {
                diag::log(&format!(
                    "wl_im: ambient display has no {IM_MANAGER_IFACE}; probing sibling sockets"
                ));
            }
        }
        Err(e) => diag::log(&format!("wl_im: no ambient Wayland display ({e})")),
    }
    if found.is_none() {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR");
        for name in sibling_sockets() {
            if state.stop.is_set() {
                return;
            }
            let Some(runtime) = runtime.as_ref() else {
                break;
            };
            let path = std::path::Path::new(runtime).join(&name);
            let stream = match UnixStream::connect(&path) {
                Ok(s) => s,
                Err(e) => {
                    diag::log(&format!(
                        "wl_im: socket '{}' unusable ({e})",
                        name.to_string_lossy()
                    ));
                    continue;
                }
            };
            let conn = match Connection::from_socket(stream) {
                Ok(c) => c,
                Err(e) => {
                    diag::log(&format!(
                        "wl_im: socket '{}' rejected the connection ({e})",
                        name.to_string_lossy()
                    ));
                    continue;
                }
            };
            if let Some(queue) = setup_im(&conn, &mut state) {
                found = Some((conn, queue, name.to_string_lossy().into_owned()));
                break;
            }
            diag::log(&format!(
                "wl_im: socket '{}' has no {IM_MANAGER_IFACE}; trying next",
                name.to_string_lossy()
            ));
        }
    }
    let Some((conn, mut queue, socket)) = found else {
        diag::log(&format!(
            "wl_im: no {IM_MANAGER_IFACE} on any Wayland display; auto-show disabled (manual toggle only)"
        ));
        return;
    };
    diag::log(&format!(
        "wl_im: {IM_MANAGER_IFACE} found on '{socket}' — text-input auto-show active"
    ));

    loop {
        if state.stop.is_set() {
            break;
        }

        // Dispatch anything already queued, without blocking.
        let _ = queue.dispatch_pending(&mut state);
        let _ = conn.flush();

        // Grace-period hide runs here, not in the render thread.
        if let Some(at) = state.hide_at
            && Instant::now() >= at
        {
            state.hide_at = None;
            state.osk.decide(OskEvent::ImDeactivate);
            continue;
        }

        // Block on the Wayland socket fd with a stop-aware timeout.
        let fd = conn.backend().poll_fd().as_raw_fd();
        let timeout_ms = match state.hide_at {
            Some(at) => {
                let ms = at.saturating_duration_since(Instant::now()).as_millis() as i32;
                ms.clamp(1, POLL_CAP_MS)
            }
            None => POLL_CAP_MS,
        };
        let mut fds = [libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: fds is a valid one-element array for the duration of the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
        if n <= 0 {
            // Timeout or spurious wakeup (EINTR) — loop re-checks stop/grace.
            continue;
        }
        if let Some(guard) = conn.prepare_read() {
            let _ = guard.read();
        }
        let _ = queue.dispatch_pending(&mut state);
    }
}
