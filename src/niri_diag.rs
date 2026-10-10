//! niri IPC diagnostics: when keebs runs as a niri child (NIRI_SOCKET is
//! inherited), ask the compositor where keyboard focus sits and which keyboard
//! devices it knows about. This tells apart the two failure modes that look
//! identical from the outside — "the OSK stole focus again" vs "niri never
//! picked up the uinput keyboard" — directly from one diag log.
//!
//! Every query is spawned on its own thread so the render loop never blocks
//! on IPC; output goes through `diag::log` (stdout + keebs-diag.log).

use crate::diag;
use std::process::Command;

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

/// Which keyboard devices niri knows (is `keebs-virtual-kb` among them?).
pub fn log_keyboards() {
    spawn_query("keyboards", true, "keyboards", 900);
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
