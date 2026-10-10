//! niri IPC diagnostics + focus guard: when keebs runs as a niri child
//! (NIRI_SOCKET is inherited), ask the compositor where keyboard focus sits
//! around OSK open/close — enough to debug focus issues from one diag log.
//!
//! It also hosts the niri FOCUS GUARD: xwayland-satellite maps every X window
//! (override-redirect included) as a managed toplevel that niri focuses on
//! map and on click, so injected keystrokes land in the OSK itself. Whenever
//! the focused window is ours, `niri msg action focus-window-previous` hands
//! focus back to the app being typed into. The guard runs synchronously
//! before every dispatch (race-free injection) and on a background poll
//! (covers the physical keyboard while the OSK is open).

use crate::diag;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// True when the niri IPC socket env is present (we are a niri child).
pub fn available() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    } else {
        s.to_string()
    }
}

fn spawn_query(label: &'static str, json: bool, subject: &'static str, trunc: usize) {
    if !available() {
        return;
    }
    std::thread::spawn(move || {
        let mut cmd = Command::new("niri");
        cmd.arg("msg");
        if json {
            cmd.arg("--json");
        }
        cmd.arg(subject);
        let msg = match cmd.output() {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() {
                    "(empty)".to_string()
                } else {
                    truncate(&s, trunc)
                }
            }
            Ok(o) => format!("FAILED: {}", String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => format!("spawn failed: {e}"),
        };
        diag::log(&format!("niri[{label}]: {msg}"));
    });
}

/// Current keyboard-focus owner, with a context label.
pub fn log_focus(label: &'static str) {
    spawn_query(label, true, "focused-window", 400);
}

/// Window list digest: how many windows niri manages, and whether anything
/// keebs/SDL/satellite-shaped is among them (a leftover managed OSK toplevel
/// would show up here even while invisible).
pub fn log_windows_digest() {
    if !available() {
        return;
    }
    std::thread::spawn(|| {
        let out = Command::new("niri")
            .arg("msg")
            .arg("--json")
            .arg("windows")
            .output();
        let Ok(o) = out else {
            return;
        };
        if !o.status.success() {
            diag::log(&format!(
                "niri[windows]: FAILED: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
            return;
        }
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        let count = s.matches("\"app_id\"").count();
        let mut hits = Vec::new();
        for needle in ["keebs", "SDL", "satellite"] {
            if let Some(pos) = s.to_lowercase().find(needle) {
                let lo = pos.saturating_sub(60);
                let hi = (lo + 260).min(s.len());
                if let Some(chunk) = s.get(lo..hi) {
                    hits.push(truncate(chunk, 240));
                }
            }
        }
        if hits.is_empty() {
            diag::log(&format!(
                "niri[windows]: {count} window(s), no keebs/SDL/satellite toplevel"
            ));
        } else {
            diag::log(&format!(
                "niri[windows]: {count} window(s), match: {}",
                hits.join(" | ")
            ));
        }
    });
}

// ---------------------------------------------------------------------------
// Focus guard — see module docs. All niri msg calls here are synchronous
// (blocking a few ms); the render loop only pays for them per key tap, and
// correctness beats latency: injecting while our own window holds focus
// drops the key into the OSK.
// ---------------------------------------------------------------------------

/// Serializes probe+restore so two threads can't double-fire
/// focus-window-previous (a second call would toggle focus BACK to the OSK).
static RESTORE_LOCK: Mutex<()> = Mutex::new(());

fn run_msg(args: &[&str]) -> Option<String> {
    let out = Command::new("niri").arg("msg").args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if !err.is_empty() {
            diag::log(&format!(
                "niri[guard]: `niri msg {}` failed: {err}",
                args.join(" ")
            ));
        }
        None
    }
}

/// Extract `"pid":<num>` from niri's JSON (field scan, no serde dep).
fn json_pid(s: &str) -> Option<u32> {
    let at = s.find("\"pid\":")? + 6;
    let rest = &s[at..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Extract `"app_id":"..."` (lowercased) from niri's JSON.
fn json_app_id(s: &str) -> Option<String> {
    let at = s.find("\"app_id\":\"")? + 10;
    let rest = &s[at..];
    let val: String = rest.chars().take_while(|c| *c != '"').collect();
    Some(val.to_lowercase())
}

/// True if the focused-window JSON describes keebs' own OSK window — matched
/// by pid (the nix wrapper execs, so the pid is ours) and, as a fallback, by
/// an app_id containing "keebs" (WM_CLASS follows the binary name).
fn looks_like_us(s: &str) -> bool {
    if json_pid(s) == Some(std::process::id()) {
        return true;
    }
    json_app_id(s).is_some_and(|id| id.contains("keebs"))
}

/// If niri currently focuses the OSK window, hand focus back to the window
/// that had it before. No-op outside niri or when focus is already elsewhere.
pub fn ensure_focus_not_ours() {
    if !available() {
        return;
    }
    // Lock held across probe+restore: the probe must be re-checked inside the
    // critical section, otherwise a queued second restore toggles back.
    let _guard = RESTORE_LOCK.lock().unwrap();
    let Some(focused) = run_msg(&["--json", "focused-window"]) else {
        return;
    };
    if focused.is_empty() || focused == "null" || !looks_like_us(&focused) {
        return;
    }
    if run_msg(&["action", "focus-window-previous"]).is_some() {
        diag::log("niri[guard]: OSK had focus -> focus-window-previous");
    }
}

/// Background poller kept alive while the OSK is visible. Restores focus even
/// when no keys are being dispatched (physical keyboard use mid-OSK).
pub struct FocusGuard {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FocusGuard {
    pub fn start() -> Self {
        if !available() {
            return Self {
                stop: Arc::new(AtomicBool::new(true)),
                handle: None,
            };
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                ensure_focus_not_ours();
                std::thread::sleep(Duration::from_millis(150));
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for FocusGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
