//! Lizard guard: neutralize the Deck's emulated mouse/keyboard at the KERNEL
//! level, without any firmware cooperation.
//!
//! Rust port of `steamcontroller/lizardguard.py` — and the reason the Python
//! app had haptics while typing: instead of disabling lizard mode (which
//! kills the firmware's native trackpad haptics), we hold `EVIOCGRAB` on
//! every Deck evdev node so its emulated mouse/keyboard events are delivered
//! ONLY to us and never reach the compositor. The firmware keeps running —
//! and keeps ticking its haptics.

use std::ffi::CString;
use std::os::fd::RawFd;
use std::path::Path;

const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
const VID: &str = "28de";
const PID: &str = "1205";

/// Paths like /dev/input/event12 whose USB ancestry is the Deck.
fn deck_event_nodes() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/input") else {
        return out;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("event"))
        .collect();
    names.sort();
    for name in names {
        let Ok(node) = fs_canonicalize(&format!("/sys/class/input/{name}/device")) else {
            continue;
        };
        // Climb toward the USB device dir looking for idVendor/idProduct
        // (the HID dir and interface dir don't carry them; the USB device
        // dir does).
        let mut d = node;
        for _ in 0..8 {
            match d.parent() {
                Some(p) if p != Path::new("/") => d = p.to_path_buf(),
                _ => break,
            }
            let vid = std::fs::read_to_string(d.join("idVendor"));
            let pid = std::fs::read_to_string(d.join("idProduct"));
            match (vid, pid) {
                (Ok(v), Ok(p)) => {
                    if v.trim().eq_ignore_ascii_case(VID) && p.trim().eq_ignore_ascii_case(PID) {
                        out.push(format!("/dev/{name}"));
                    }
                    break;
                }
                _ => continue, // climb further
            }
        }
    }
    out
}

fn fs_canonicalize(p: &str) -> std::io::Result<std::path::PathBuf> {
    std::fs::canonicalize(p)
}

/// Hold/release EVIOCGRAB on all of the Deck's evdev nodes.
#[derive(Default)]
pub struct LizardGrabber {
    fds: Vec<RawFd>,
}

impl LizardGrabber {
    pub fn engaged(&self) -> bool {
        !self.fds.is_empty()
    }

    /// Grab everything (idempotent). Returns the number of nodes held.
    pub fn engage(&mut self) -> usize {
        if !self.fds.is_empty() {
            return self.fds.len();
        }
        let mut warned = false;
        for path in deck_event_nodes() {
            let Ok(c) = CString::new(path.clone()) else {
                continue;
            };
            let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
            if fd < 0 {
                if !warned {
                    warned = true;
                    eprintln!(
                        "lizardguard: cannot grab {path} — fix once with: \
                         sudo usermod -aG input $USER then log out/in \
                         (until then the Deck pads move the desktop mouse \
                         while the keyboard is open)"
                    );
                }
                continue;
            }
            let rc = unsafe { libc::ioctl(fd, EVIOCGRAB, 1 as libc::c_int) };
            if rc == 0 {
                self.fds.push(fd);
            } else {
                unsafe {
                    libc::close(fd);
                }
            }
        }
        if !self.fds.is_empty() {
            println!(
                "lizardguard: holding {} Deck input node(s) — firmware \
                 mouse/keys swallowed, native haptics stay alive",
                self.fds.len()
            );
        }
        self.fds.len()
    }

    pub fn release(&mut self) {
        while let Some(fd) = self.fds.pop() {
            unsafe {
                libc::ioctl(fd, EVIOCGRAB, 0 as libc::c_int);
                libc::close(fd);
            }
        }
    }
}

impl Drop for LizardGrabber {
    fn drop(&mut self) {
        self.release();
    }
}
