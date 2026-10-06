//! Minimal libusb-1.0 binding via dlopen for feature-report SET_REPORT
//! control transfers — the *verified-working* lizard-disable path from the
//! Python driver (`steamcontroller/__init__.py::_send_lizard_via_libusb`).
//!
//! The kernel/hidraw paths don't reliably reach the Deck firmware's
//! lizard-mode setting on this hardware; the libusb control transfer with
//! the exact wire format does: report ID in wValue only, payload = the 64
//! body bytes WITHOUT the id (the hidapi-libusb backend got this wrong,
//! which is why the Python driver hand-rolled it).

use libloading::Library;
use std::os::raw::c_void;

type InitFn = unsafe extern "C" fn(*mut *mut c_void) -> i32;
type OpenFn = unsafe extern "C" fn(*mut c_void, u16, u16) -> *mut c_void;
type IntFn = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type CtrlFn = unsafe extern "C" fn(*mut c_void, u8, u8, u16, u16, *mut u8, u16, u32) -> i32;
type CloseFn = unsafe extern "C" fn(*mut c_void);
type ExitFn = unsafe extern "C" fn(*mut c_void);

/// An open libusb device with the vendor interface claimed for the lifetime
/// of the session (Python: deliberately never re-attach the kernel driver —
/// re-attach puts the interface in a transient state that breaks subsequent
/// opens, and the kernel driver re-binds on process exit anyway).
pub struct LibusbSession {
    /// Holds libusb loaded for the session's lifetime (functions are used
    /// from it on every send/drop).
    _lib: Library,
    ctx: *mut c_void,
    handle: *mut c_void,
    control: CtrlFn,
    release: IntFn,
    close: CloseFn,
    exit: ExitFn,
    iface: i32,
}

unsafe impl Send for LibusbSession {}

impl LibusbSession {
    /// Open the device and claim `iface` (detaching usbhid from it first, if
    /// attached). Returns None if libusb is missing or the device/interface
    /// can't be taken — callers fall back to the hidapi/hidraw paths.
    pub fn open(vendor_id: u16, product_id: u16, iface: i32) -> Option<Self> {
        unsafe {
            let lib = Library::new("libusb-1.0.so.0")
                .or_else(|_| Library::new("libusb-1.0.so"))
                .ok()?;
            let init: InitFn = *lib.get(b"libusb_init\0").ok()?;
            let open: OpenFn = *lib.get(b"libusb_open_device_with_vid_pid\0").ok()?;
            let active: IntFn = *lib.get(b"libusb_kernel_driver_active\0").ok()?;
            let detach: IntFn = *lib.get(b"libusb_detach_kernel_driver\0").ok()?;
            let claim: IntFn = *lib.get(b"libusb_claim_interface\0").ok()?;
            let release: IntFn = *lib.get(b"libusb_release_interface\0").ok()?;
            let control: CtrlFn = *lib.get(b"libusb_control_transfer\0").ok()?;
            let close: CloseFn = *lib.get(b"libusb_close\0").ok()?;
            let exit: ExitFn = *lib.get(b"libusb_exit\0").ok()?;

            let mut ctx: *mut c_void = std::ptr::null_mut();
            if init(&mut ctx) != 0 {
                return None;
            }
            let handle = open(ctx, vendor_id, product_id);
            if handle.is_null() {
                exit(ctx);
                return None;
            }

            // Detach usbhid from the vendor interface, then claim it.
            if active(handle, iface) == 1 {
                detach(handle, iface);
            }
            if claim(handle, iface) != 0 {
                close(handle);
                exit(ctx);
                return None;
            }

            Some(Self {
                _lib: lib,
                ctx,
                handle,
                control,
                release,
                close,
                exit,
                iface,
            })
        }
    }

    /// Send a 65-byte feature report (report id at [0]) as a class
    /// SET_REPORT control transfer on the claimed vendor interface.
    /// Returns true when the firmware accepted all 64 payload bytes.
    pub fn send_feature(&self, report: &[u8; 65]) -> bool {
        let report_id = report[0] as u16;
        unsafe {
            let rc = (self.control)(
                self.handle,
                0x21,                    // class | interface | host-to-device
                0x09,                    // SET_REPORT
                (0x03 << 8) | report_id, // Feature report
                self.iface as u16,
                report.as_ptr().add(1) as *mut u8, // body WITHOUT the id
                64,
                1000,
            );
            rc == 64
        }
    }
}

impl Drop for LibusbSession {
    fn drop(&mut self) {
        // Release (no re-attach, per the Python driver's note) and tear down.
        unsafe {
            (self.release)(self.handle, self.iface);
            (self.close)(self.handle);
            (self.exit)(self.ctx);
        }
    }
}
