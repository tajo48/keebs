//! Lizard guard: neutralize a controller's emulated mouse/keyboard at the
//! KERNEL level, without any firmware cooperation.
//!
//! Background: feature-report lizard disables are unreliable — on the Deck
//! the firmware ignores them entirely, and on the Steam Controller 2026 the
//! write reportedly persists in firmware until a power-cycle (see
//! `triton.rs`). But the emulated input enters the system as ordinary evdev
//! nodes created by the kernel's generic HID driver on the controller's
//! keyboard/mouse interfaces. The EVIOCGRAB ioctl makes the kernel deliver
//! a device's events ONLY to the grabbing file descriptor — the compositor
//! stops seeing them entirely. One grab == lizard "off" from the desktop's
//! point of view; one release == instantly back on. Our own input path reads
//! the vendor data interface via hidraw/libusb, which EVIOCGRAB does not
//! affect.
//!
//! Parameterized by USB product ids: the Deck uses 1205, the SC 2026
//! dongle/wired use 1304/1302.
//!
//! Permission note: /dev/input/event* are root:input. Without write access
//! the grab fails; we then print a one-time remediation hint.

use std::ffi::CString;
use std::os::fd::RawFd;
use std::path::Path;

const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
const VID: &str = "28de";

/// Paths like /dev/input/event12 whose USB ancestry is one of `pids`.
fn event_nodes_for(pids: &[String]) -> Vec<String> {
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
                    if v.trim().eq_ignore_ascii_case(VID)
                        && pids.iter().any(|want| p.trim().eq_ignore_ascii_case(want))
                    {
                        out.push(format!("/dev/input/{name}"));
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

/// Hold/release EVIOCGRAB on all of a device family's evdev nodes.
pub struct LizardGrabber {
    pids: Vec<String>,
    fds: Vec<RawFd>,
}

impl LizardGrabber {
    /// `pids`: lowercase hex USB product ids ("1205", "1304", …) whose
    /// evdev nodes this grabber owns.
    pub fn new(pids: &[&str]) -> Self {
        Self {
            pids: pids.iter().map(|s| s.to_string()).collect(),
            fds: Vec::new(),
        }
    }

    pub fn engaged(&self) -> bool {
        !self.fds.is_empty()
    }

    /// Grab everything (idempotent). Returns the number of nodes held.
    pub fn engage(&mut self) -> usize {
        if !self.fds.is_empty() {
            return self.fds.len();
        }
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let label = self.pids.join("/");
        for path in event_nodes_for(&self.pids) {
            let Ok(c) = CString::new(path.clone()) else {
                continue;
            };
            let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
            if fd < 0 {
                if !WARNED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    eprintln!(
                        "lizardguard: cannot open {path}: {} — fix once with: \
                         sudo usermod -aG input $USER then log out/in \
                         (until then this controller's firmware mouse/keys \
                         keep reaching the desktop)",
                        std::io::Error::last_os_error()
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
                "lizardguard: holding {} input node(s) for 28de:{label} — firmware \
                 mouse/keys swallowed at the kernel level",
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
