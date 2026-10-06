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
type GetDeviceFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type GetConfigFn = unsafe extern "C" fn(*mut c_void, u8, *mut *mut ConfigDesc) -> i32;
type FreeCfgFn = unsafe extern "C" fn(*mut c_void);
type IntrFn = unsafe extern "C" fn(*mut c_void, u8, *mut u8, i32, *mut i32, u32) -> i32;

// Partial libusb descriptor structs (only the fields we walk; layout matches
// libusb.h on 64-bit).
#[repr(C)]
struct ConfigDesc {
    _b_length: u8,
    _b_desc_type: u8,
    _w_total_length: u16,
    b_num_interfaces: u8,
    _b_config_value: u8,
    _i_config: u8,
    _bm_attributes: u8,
    _max_power: u8,
    interface: *const Interface,
}

#[repr(C)]
struct Interface {
    altsetting: *const InterfaceDesc,
    num_altsetting: i32,
}

#[repr(C)]
struct InterfaceDesc {
    _b_length: u8,
    _b_desc_type: u8,
    b_interface_number: u8,
    _b_alt_setting: u8,
    b_num_endpoints: u8,
    _class: u8,
    _subclass: u8,
    _protocol: u8,
    _i_iface: u8,
    endpoint: *const EndpointDesc,
}

#[repr(C)]
struct EndpointDesc {
    _b_length: u8,
    _b_desc_type: u8,
    b_endpoint_address: u8,
    _bm_attributes: u8,
    _w_max_packet_size: u16,
    _b_interval: u8,
    _b_refresh: u8,
    _b_sync_address: u8,
}

/// An open libusb device with the vendor interface claimed for the lifetime
/// of the session (Python: deliberately never re-attach the kernel driver —
/// re-attach puts the interface in a transient state that breaks subsequent
/// opens, and the kernel driver re-binds on process exit anyway).
pub struct LibusbSession {
    /// Holds libusb loaded for the session's lifetime (functions are used
    /// from it on every send/read/drop).
    _lib: Library,
    ctx: *mut c_void,
    handle: *mut c_void,
    control: CtrlFn,
    interrupt: IntrFn,
    release: IntFn,
    close: CloseFn,
    exit: ExitFn,
    iface: i32,
    read_endpoint: Option<u8>,
    write_endpoint: Option<u8>,
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
            let get_device: GetDeviceFn = *lib.get(b"libusb_get_device\0").ok()?;
            let get_config: GetConfigFn = *lib.get(b"libusb_get_config_descriptor\0").ok()?;
            let free_config: FreeCfgFn = *lib.get(b"libusb_free_config_descriptor\0").ok()?;
            let interrupt: IntrFn = *lib.get(b"libusb_interrupt_transfer\0").ok()?;

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

            // Discover the interrupt IN/OUT endpoints of the vendor interface
            // (config 0). The IN endpoint carries the 64-byte input frames.
            let (mut read_ep, mut write_ep) = (None, None);
            let dev = get_device(handle);
            let mut config: *mut ConfigDesc = std::ptr::null_mut();
            if get_config(dev, 0, &mut config) == 0 && !config.is_null() {
                let cfg = &*config;
                if (iface as usize) < cfg.b_num_interfaces as usize {
                    let iface_desc = &*cfg.interface.add(iface as usize);
                    if iface_desc.num_altsetting > 0 {
                        let alt = &*iface_desc.altsetting;
                        for e in 0..alt.b_num_endpoints as usize {
                            let ep = &*alt.endpoint.add(e);
                            if ep.b_endpoint_address & 0x80 != 0 {
                                read_ep = Some(ep.b_endpoint_address);
                            } else {
                                write_ep = Some(ep.b_endpoint_address);
                            }
                        }
                    }
                }
                free_config(config as *mut c_void);
            }

            Some(Self {
                _lib: lib,
                ctx,
                handle,
                control,
                interrupt,
                release,
                close,
                exit,
                iface,
                read_endpoint: read_ep,
                write_endpoint: write_ep,
            })
        }
    }

    /// Read one interrupt-IN packet (a raw input report, report id included)
    /// with a timeout. `Ok(0)` = quiet timeout; `Err` = device gone.
    pub fn read_timeout(&self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, ()> {
        let Some(ep) = self.read_endpoint else {
            return Err(());
        };
        let mut transferred: i32 = 0;
        let rc = unsafe {
            (self.interrupt)(
                self.handle,
                ep,
                buf.as_mut_ptr(),
                buf.len() as i32,
                &mut transferred,
                timeout_ms,
            )
        };
        if rc == 0 {
            Ok(transferred.max(0) as usize)
        } else if rc == -7 {
            Ok(0) // LIBUSB_ERROR_TIMEOUT
        } else {
            Err(())
        }
    }

    /// Write one interrupt-OUT packet (e.g. a 65-byte haptic output report).
    pub fn write(&self, data: &[u8]) -> bool {
        let Some(ep) = self.write_endpoint else {
            return false;
        };
        let mut transferred: i32 = 0;
        let rc = unsafe {
            (self.interrupt)(
                self.handle,
                ep,
                data.as_ptr() as *mut u8,
                data.len() as i32,
                &mut transferred,
                1000,
            )
        };
        rc == 0 && transferred as usize == data.len()
    }

    /// Send a 65-byte haptic OUTPUT report over every plausible channel —
    /// the Python deck driver's triple: a full interrupt-OUT write, a
    /// 64-byte (unnumbered) interrupt-OUT write, and a class SET_REPORT
    /// (OUTPUT type) control transfer. Duplicates are benign (same tone
    /// restarted); we can't detect which channel the firmware honors.
    pub fn send_output(&self, report: &[u8; 65]) -> bool {
        let mut ok = self.write(report);
        if !ok {
            ok = self.write(&report[..64]);
        }
        let rc = unsafe {
            (self.control)(
                self.handle,
                0x21,                           // class | interface | host-to-device
                0x09,                           // SET_REPORT
                (0x02 << 8) | report[0] as u16, // OUTPUT report
                self.iface as u16,
                report.as_ptr().add(1) as *mut u8,
                64,
                1000,
            )
        };
        ok || rc == 64
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
