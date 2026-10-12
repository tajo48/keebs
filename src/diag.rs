//! Diagnostic tee: high-value runtime lines go to stdout AND append to
//! `./keebs-diag.log` in the working directory, so a misbehaving session can
//! be diagnosed directly from the file even when terminal output never gets
//! shared. Every line is stamped with seconds since process start, so the
//! log shows not just WHICH init stage retried but HOW LONG it starved.
//! Run keebs from the project directory and the log lands next to the
//! sources.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

const LOG: &str = "keebs-diag.log";

static T0: OnceLock<Instant> = OnceLock::new();

fn t0() -> &'static Instant {
    T0.get_or_init(Instant::now)
}

/// Print to stdout and append to the diag log (best effort), stamped with
/// elapsed seconds since `start()`.
pub fn log(msg: &str) {
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

/// Fresh log per process run. Also anchors the timestamp origin.
pub fn start() {
    let _ = t0();
    let _ = std::fs::write(LOG, format!("--- keebs pid {} ---\n", std::process::id()));
}
