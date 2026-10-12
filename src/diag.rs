//! Diagnostic tee for session-start / init debugging. OFF by default —
//! normal runs must not write files or spam stdout. Set KEEBS_DIAG=1 to
//! enable: high-value runtime lines then go to stdout AND append to
//! `./keebs-diag.log` in the working directory ($HOME when GNOME-autostarted),
//! each stamped with seconds since process start so the log shows not just
//! WHICH init stage retried but HOW LONG it starved.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

const LOG: &str = "keebs-diag.log";

static T0: OnceLock<Instant> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

fn t0() -> &'static Instant {
    T0.get_or_init(Instant::now)
}

fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var("KEEBS_DIAG").is_ok_and(|v| v == "1"))
}

/// Print to stdout and append to the diag log (best effort), stamped with
/// elapsed seconds since `start()`. No-op unless KEEBS_DIAG=1.
pub fn log(msg: &str) {
    if !enabled() {
        return;
    }
    let line = format!("[{:>7.3}] {msg}", t0().elapsed().as_secs_f32());
    println!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Fresh log per process run (only when KEEBS_DIAG=1). Also anchors the
/// timestamp origin in all modes.
pub fn start() {
    let _ = t0();
    if enabled() {
        let _ = std::fs::write(LOG, format!("--- keebs pid {} ---\n", std::process::id()));
    }
}
