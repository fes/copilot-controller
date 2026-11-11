//! macOS ARM64 HID backend over IOKit's IOHIDManager and CoreFoundation.
//! All raw FFI is private to this file. Not validated against hardware.
//!
//! Input reports are delivered through a run-loop callback on the thread that
//! opened the device (the transport is therefore `!Send`). Writes are deliberately
//! unavailable: `IOHIDDeviceSetReport` is synchronous with no caller deadline and
//! no supervised worker exists on macOS, so no write FFI is declared here.

use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_void};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use super::*;

type CFTypeRef = *const c_void;
type CFIndex = isize;
type IOReturn = i32;

const UTF8: u32 = 0x0800_0100;
const SINT32: CFIndex = 3;
const MAX_QUEUED: usize = 64;
const MAX_SCAN: CFIndex = 4096;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorDefault: CFTypeRef;
    static kCFRunLoopDefaultMode: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: CFTypeRef) -> CFIndex;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFNumberGetValue(n: CFTypeRef, ty: CFIndex, out: *mut c_void) -> u8;
    fn CFNumberCreate(a: CFTypeRef, ty: CFIndex, v: *const c_void) -> CFTypeRef;
    fn CFStringCreateWithCString(a: CFTypeRef, s: *const c_char, enc: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, enc: u32) -> u8;
    fn CFDictionaryCreate(
        a: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: CFIndex,
        kcb: *const u8,
        vcb: *const u8,
    ) -> CFTypeRef;
    fn CFSetGetCount(s: CFTypeRef) -> CFIndex;
    fn CFSetGetValues(s: CFTypeRef, out: *mut CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFTypeRef;
    fn CFRunLoopRunInMode(mode: CFTypeRef, secs: f64, return_after_source: u8) -> i32;
}

type InputCb = extern "C" fn(*mut c_void, IOReturn, *mut c_void, u32, u32, *mut u8, CFIndex);

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(a: CFTypeRef, options: u32) -> CFTypeRef;
    fn IOHIDManagerSetDeviceMatching(m: CFTypeRef, matching: CFTypeRef);
    fn IOHIDManagerCopyDevices(m: CFTypeRef) -> CFTypeRef;
    fn IOHIDDeviceGetProperty(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn IOHIDDeviceGetService(d: CFTypeRef) -> u32;
    fn IOHIDDeviceOpen(d: CFTypeRef, options: u32) -> IOReturn;
    fn IOHIDDeviceClose(d: CFTypeRef, options: u32) -> IOReturn;
    fn IOHIDDeviceRegisterInputReportCallback(
        d: CFTypeRef,
        report: *mut u8,
        len: CFIndex,
        cb: Option<InputCb>,
        ctx: *mut c_void,
    );
    fn IOHIDDeviceScheduleWithRunLoop(d: CFTypeRef, rl: CFTypeRef, mode: CFTypeRef);
    fn IOHIDDeviceUnscheduleFromRunLoop(d: CFTypeRef, rl: CFTypeRef, mode: CFTypeRef);
    fn IORegistryEntryGetRegistryEntryID(service: u32, id: *mut u64) -> i32;
}

/// Owned CoreFoundation object released on drop.
struct Cf(CFTypeRef);

impl Cf {
    fn new(p: CFTypeRef) -> Option<Self> {
        (!p.is_null()).then_some(Self(p))
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: owned +1 reference.
        unsafe { CFRelease(self.0) };
    }
}

fn cfstr(s: &str) -> Option<Cf> {
    let c = CString::new(s).ok()?;
    // SAFETY: valid NUL-terminated string.
    Cf::new(unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), UTF8) })
}

fn cfnum(v: i32) -> Option<Cf> {
    // SAFETY: `v` is a valid SInt32 source.
    Cf::new(unsafe { CFNumberCreate(kCFAllocatorDefault, SINT32, (&raw const v).cast()) })
}

fn prop_i32(dev: CFTypeRef, key: &str) -> Option<i32> {
    let k = cfstr(key)?;
    // SAFETY: borrowed (get-rule) property; type verified before use.
    let v = unsafe { IOHIDDeviceGetProperty(dev, k.0) };
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out = 0i32;
    // SAFETY: `v` is a CFNumber and `out` a valid SInt32 destination.
    (unsafe { CFNumberGetValue(v, SINT32, (&raw mut out).cast()) } != 0).then_some(out)
}

fn prop_string(dev: CFTypeRef, key: &str) -> Option<String> {
    let k = cfstr(key)?;
    // SAFETY: borrowed property; type verified before use.
    let v = unsafe { IOHIDDeviceGetProperty(dev, k.0) };
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    // SAFETY: buffer pointer/length describe `buf`.
    if unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as CFIndex, UTF8) } == 0 {
        return None;
    }
    // SAFETY: CFStringGetCString NUL-terminates on success.
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    (!s.is_empty()).then_some(s)
}

fn entry_id(dev: CFTypeRef) -> Option<u64> {
    // SAFETY: borrowed device; out-pointer is valid.
    let svc = unsafe { IOHIDDeviceGetService(dev) };
    if svc == 0 {
        return None;
    }
    let mut id = 0u64;
    (unsafe { IORegistryEntryGetRegistryEntryID(svc, &mut id) } == 0).then_some(id)
}

/// Manager restricted to one vendor/product, plus the (owned) matching devices.
struct Matched {
    _manager: Cf,
    set: Cf,
}

impl Matched {
    fn new(id: HidIdentity) -> Result<Self, HidError> {
        let err = HidError::Os {
            op: HidOp::Enumerate,
            code: 0,
        };
        let (kv, kp) = (
            cfstr("VendorID").ok_or(err.clone())?,
            cfstr("ProductID").ok_or(err.clone())?,
        );
        let (vv, vp) = (
            cfnum(i32::from(id.vendor_id)).ok_or(err.clone())?,
            cfnum(i32::from(id.product_id)).ok_or(err.clone())?,
        );
        let keys = [kv.0, kp.0];
        let vals = [vv.0, vp.0];
        // SAFETY: arrays hold 2 valid CF objects; callbacks are the CF-provided statics.
        let dict = Cf::new(unsafe {
            CFDictionaryCreate(
                kCFAllocatorDefault,
                keys.as_ptr(),
                vals.as_ptr(),
                2,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        })
        .ok_or(err.clone())?;
        // SAFETY: default allocator, no options. The manager is deliberately never opened.
        let manager =
            Cf::new(unsafe { IOHIDManagerCreate(kCFAllocatorDefault, 0) }).ok_or(err.clone())?;
        // SAFETY: valid manager and dictionary.
        unsafe { IOHIDManagerSetDeviceMatching(manager.0, dict.0) };
        // SAFETY: valid manager; result follows the copy rule (owned) and may be NULL.
        let set = Cf::new(unsafe { IOHIDManagerCopyDevices(manager.0) });
        match set {
            Some(set) => Ok(Self {
                _manager: manager,
                set,
            }),
            None => Err(HidError::NotFound),
        }
    }

    fn devices(&self) -> Result<Vec<CFTypeRef>, HidError> {
        // SAFETY: valid set.
        let n = unsafe { CFSetGetCount(self.set.0) };
        if !(0..=MAX_SCAN).contains(&n) {
            return Err(HidError::Protocol("implausible device count"));
        }
        let mut v = vec![null(); n as usize];
        // SAFETY: `v` has room for `n` values.
        unsafe { CFSetGetValues(self.set.0, v.as_mut_ptr()) };
        Ok(v)
    }
}

pub(super) struct IoKitHid;

fn path_for(id: u64) -> String {
    format!("iokit:{id}")
}

fn describe(dev: CFTypeRef, identity: HidIdentity) -> Option<HidDescriptor> {
    let id = entry_id(dev)?;
    let size = |k| {
        prop_i32(dev, k)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0)
    };
    let page = |k| {
        prop_i32(dev, k)
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(0)
    };
    Some(HidDescriptor {
        identity,
        usage_page: page("PrimaryUsagePage"),
        usage: page("PrimaryUsage"),
        product: prop_string(dev, "Product"),
        serial: prop_string(dev, "SerialNumber"),
        max_input_report_len: size("MaxInputReportSize"),
        max_output_report_len: size("MaxOutputReportSize"),
        interface_number: prop_i32(dev, "InterfaceNumber"),
        bus: prop_string(dev, "Transport").map(|transport| {
            if transport.eq_ignore_ascii_case("usb") {
                HidBus::Usb
            } else if transport.eq_ignore_ascii_case("bluetooth") {
                HidBus::Bluetooth
            } else {
                HidBus::Unknown
            }
        }),
        path: HidPath::new(path_for(id)).ok()?,
    })
}

fn same_open_identity(
    reopened: &HidDescriptor,
    selected: &HidDescriptor,
    allow_usage_change: bool,
) -> bool {
    let usage_matches = allow_usage_change
        || (reopened.usage_page == selected.usage_page && reopened.usage == selected.usage);
    reopened.identity == selected.identity
        && reopened.interface_number == selected.interface_number
        && reopened.bus == selected.bus
        && reopened.path == selected.path
        && usage_matches
}

impl HidBackend for IoKitHid {
    fn enumerate(&self, m: &HidMatcher, limit: usize) -> Result<Vec<HidDescriptor>, HidError> {
        let matched = match Matched::new(m.identity) {
            Ok(x) => x,
            Err(HidError::NotFound) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut found = Vec::new();
        for dev in matched.devices()? {
            let Some(d) = describe(dev, m.identity) else {
                continue;
            };
            if m.matches(&d) {
                if found.len() == limit {
                    return Err(HidError::TooManyDevices);
                }
                found.push(d);
            }
        }
        Ok(found)
    }

    fn open(&self, d: &HidDescriptor) -> Result<Box<dyn HidTransport>, HidError> {
        self.open_with_policy(d, false)
    }

    fn open_allowing_usage_change(
        &self,
        d: &HidDescriptor,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        self.open_with_policy(d, true)
    }
}

impl IoKitHid {
    fn open_with_policy(
        &self,
        d: &HidDescriptor,
        allow_usage_change: bool,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        let matched = Matched::new(d.identity)?;
        let dev = matched
            .devices()?
            .into_iter()
            .find(|&x| {
                describe(x, d.identity)
                    .is_some_and(|c| same_open_identity(&c, d, allow_usage_change))
            })
            .ok_or(HidError::NotFound)?;
        // SAFETY: `dev` is a live element of the matched set; retained for our lifetime.
        let dev = unsafe { CFRetain(dev) };
        let dev = Cf(dev);
        // SAFETY: valid device; no seize option.
        let r = unsafe { IOHIDDeviceOpen(dev.0, 0) };
        if r != 0 {
            return Err(HidError::Os {
                op: HidOp::Open,
                code: i64::from(r),
            });
        }
        let mut state = Box::new(InputState {
            buf: vec![0u8; MAX_REPORT_LEN],
            queue: VecDeque::new(),
        });
        let ctx = (&raw mut *state).cast::<c_void>();
        // SAFETY: `state` (and its fixed-size buffer) outlives the registration: Drop
        // unregisters before freeing. The callback only touches `*ctx`.
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                dev.0,
                state.buf.as_mut_ptr(),
                MAX_REPORT_LEN as CFIndex,
                Some(on_input),
                ctx,
            );
            IOHIDDeviceScheduleWithRunLoop(dev.0, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
        }
        let report_descriptor = original_report_descriptor(dev.0);
        Ok(Box::new(MacTransport {
            dev,
            _matched: matched,
            run_loop: unsafe { CFRunLoopGetCurrent() },
            state,
            report_descriptor,
        }))
    }
}

struct InputState {
    buf: Vec<u8>,
    queue: VecDeque<Vec<u8>>,
}

extern "C" fn on_input(
    ctx: *mut c_void,
    result: IOReturn,
    _sender: *mut c_void,
    _ty: u32,
    id: u32,
    report: *mut u8,
    len: CFIndex,
) {
    if result != 0 || ctx.is_null() || report.is_null() || len <= 0 || len as usize > MAX_REPORT_LEN
    {
        return;
    }
    // SAFETY: ctx is the InputState registered at open and alive until unregistered;
    // IOKit guarantees `report` is valid for `len` bytes during the callback.
    let (state, data) = unsafe {
        (
            &mut *ctx.cast::<InputState>(),
            std::slice::from_raw_parts(report, len as usize),
        )
    };
    if state.queue.len() >= MAX_QUEUED {
        return;
    }
    let mut out = Vec::with_capacity(data.len() + 1);
    // Normalise to Windows-style framing: report-ID byte first (0 if unnumbered).
    if data.first().copied().map(u32::from) != Some(id) || id == 0 {
        out.push(id as u8);
    }
    out.extend_from_slice(data);
    if out.len() <= MAX_REPORT_LEN {
        state.queue.push_back(out);
    }
}

struct MacTransport {
    dev: Cf,
    _matched: Matched,
    run_loop: CFTypeRef,
    state: Box<InputState>,
    report_descriptor: Option<Vec<u8>>,
}

fn original_report_descriptor(dev: CFTypeRef) -> Option<Vec<u8>> {
    let key = cfstr("ReportDescriptor")?;
    // SAFETY: borrowed property from a live IOHIDDevice.
    let data = unsafe { IOHIDDeviceGetProperty(dev, key.0) };
    if data.is_null() || unsafe { CFGetTypeID(data) != CFDataGetTypeID() } {
        return None;
    }
    let len = unsafe { CFDataGetLength(data) };
    if !(1..=1024).contains(&len) {
        return None;
    }
    let ptr = unsafe { CFDataGetBytePtr(data) };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: CFData owns `len` contiguous bytes and remains alive for this copy.
    Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec())
}

impl Drop for MacTransport {
    fn drop(&mut self) {
        // SAFETY: unregister callbacks before `state` is freed, then unschedule and close.
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                self.dev.0,
                self.state.buf.as_mut_ptr(),
                MAX_REPORT_LEN as CFIndex,
                None,
                null_mut(),
            );
            IOHIDDeviceUnscheduleFromRunLoop(self.dev.0, self.run_loop, kCFRunLoopDefaultMode);
            IOHIDDeviceClose(self.dev.0, 0);
        }
    }
}

impl HidTransport for MacTransport {
    fn supports_write(&self) -> bool {
        false
    }

    fn write(&mut self, _report: &[u8]) -> Result<usize, HidError> {
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    }

    fn read(&mut self, out: &mut [u8], timeout: Duration) -> Result<usize, HidError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = self.state.queue.pop_front() {
                let n = r.len().min(out.len());
                out[..n].copy_from_slice(&r[..n]);
                return Ok(n);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(HidError::Timeout);
            }
            // SAFETY: services this thread's run loop for at most `left`.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, left.as_secs_f64(), 1) };
        }
    }

    fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        self.report_descriptor
            .clone()
            .ok_or(HidError::EvidenceUnavailable(
                EvidenceKind::RawReportDescriptor,
            ))
    }
}
