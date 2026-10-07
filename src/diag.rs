//! Diagnostic tee: high-value runtime lines go to stdout AND append to
//! `./keebs-diag.log` in the working directory, so a misbehaving session can
//! be diagnosed directly from the file even when terminal output never gets
//! shared. Run keebs from the project directory and the log lands next to
//! the sources.

use std::io::Write;

const LOG: &str = "keebs-diag.log";

/// Print to stdout and append to the diag log (best effort).
pub fn log(msg: &str) {
    println!("{msg}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG)
    {
        let _ = writeln!(f, "{msg}");
    }
}

/// Fresh log per process run.
pub fn start() {
    let _ = std::fs::write(LOG, format!("--- keebs pid {} ---\n", std::process::id()));
}
