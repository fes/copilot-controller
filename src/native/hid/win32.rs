//! Windows HID backend over SetupAPI, hid.dll, and overlapped file I/O.
//! All raw FFI is private to this file.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid,
    HidD_GetPreparsedData, HidD_GetProductString, HidD_GetSerialNumberString, HidP_GetCaps,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::platform::parse_vid_pid;
use super::*;

const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;
/// Fixed write deadline; cancellation is best effort and unvalidated on hardware.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const STRING_BYTES: usize = 256;

pub(super) struct WindowsHid;

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateFileW/CreateEventW and is owned here.
        unsafe { CloseHandle(self.0) };
    }
}

struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: owned set returned by SetupDiGetClassDevsW.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn os_err(op: HidOp) -> HidError {
    // SAFETY: trivially safe thread-local error read.
    HidError::Os {
        op,
        code: i64::from(unsafe { GetLastError() }),
    }
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn open_handle(path: &str, access: u32, flags: u32) -> Option<Handle> {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            flags,
            null_mut(),
        )
    };
    (h != INVALID_HANDLE_VALUE).then_some(Handle(h))
}

fn attributes(h: &Handle) -> Option<HidIdentity> {
    let mut a = HIDD_ATTRIBUTES {
        Size: size_of::<HIDD_ATTRIBUTES>() as u32,
        VendorID: 0,
        ProductID: 0,
        VersionNumber: 0,
    };
    // SAFETY: `a` is a valid, correctly sized out-structure.
    unsafe { HidD_GetAttributes(h.0, &mut a) }.then_some(HidIdentity {
        vendor_id: a.VendorID,
        product_id: a.ProductID,
    })
}

fn caps(h: &Handle) -> Option<HIDP_CAPS> {
    let mut pre = 0isize;
    // SAFETY: out-pointer is valid; the preparsed data is freed below on every path.
    if !unsafe { HidD_GetPreparsedData(h.0, &mut pre) } {
        return None;
    }
    let mut c = HIDP_CAPS::default();
    // SAFETY: `pre` is valid preparsed data and `c` a valid out-structure.
    let st = unsafe { HidP_GetCaps(pre, &mut c) };
    // SAFETY: `pre` came from HidD_GetPreparsedData and is not used afterwards.
    unsafe { HidD_FreePreparsedData(pre) };
    (st == HIDP_STATUS_SUCCESS).then_some(c)
}

fn string_attr(
    h: &Handle,
    f: unsafe extern "system" fn(HANDLE, *mut core::ffi::c_void, u32) -> bool,
) -> Option<String> {
    let mut buf = [0u16; STRING_BYTES / 2];
    // SAFETY: buffer pointer/length describe `buf` exactly.
    if !unsafe { f(h.0, buf.as_mut_ptr().cast(), STRING_BYTES as u32) } {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..end]);
    (!s.is_empty()).then_some(s)
}

fn interface_paths(limit_scan: usize) -> Result<Vec<String>, HidError> {
    let mut guid = windows_sys::core::GUID {
        data1: 0,
        data2: 0,
        data3: 0,
        data4: [0; 8],
    };
    // SAFETY: valid out-pointer.
    unsafe { HidD_GetHidGuid(&mut guid) };
    // SAFETY: GUID pointer is valid; null enumerator/parent are permitted.
    let set = unsafe {
        SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set as isize == -1 {
        return Err(os_err(HidOp::Enumerate));
    }
    let set = DevInfo(set);
    let mut out = Vec::new();
    for index in 0..limit_scan as u32 {
        let mut ifd: SP_DEVICE_INTERFACE_DATA = unsafe { core::mem::zeroed() };
        ifd.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        // SAFETY: `ifd` is initialised with cbSize as required.
        if unsafe { SetupDiEnumDeviceInterfaces(set.0, null(), &guid, index, &mut ifd) } == 0 {
            break;
        }
        let mut needed = 0u32;
        // SAFETY: size query with a null buffer.
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(set.0, &ifd, null_mut(), 0, &mut needed, null_mut())
        };
        let needed = needed as usize;
        if needed < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() || needed > 4096 {
            continue;
        }
        // u32-aligned buffer, as the structure requires.
        let mut buf = vec![0u32; needed.div_ceil(4)];
        let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        // SAFETY: buffer is at least `needed` bytes and suitably aligned.
        unsafe { (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 };
        // SAFETY: as above; `needed` bounds the write.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &ifd,
                detail,
                needed as u32,
                null_mut(),
                null_mut(),
            )
        };
        if ok == 0 {
            continue;
        }
        // SAFETY: DevicePath is a NUL-terminated wide string inside `buf`.
        let path_ptr = unsafe { core::ptr::addr_of!((*detail).DevicePath) }.cast::<u16>();
        let max =
            (needed - core::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath)) / 2;
        // SAFETY: bounded by the buffer's remaining capacity.
        let slice = unsafe { core::slice::from_raw_parts(path_ptr, max) };
        let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        out.push(String::from_utf16_lossy(&slice[..end]));
    }
    Ok(out)
}

impl HidBackend for WindowsHid {
    fn enumerate(&self, m: &HidMatcher, limit: usize) -> Result<Vec<HidDescriptor>, HidError> {
        let mut found = Vec::new();
        for path in interface_paths(4096)? {
            // Only paths whose name carries the requested identity are touched.
            if parse_vid_pid(&path) != Some(m.identity) {
                continue;
            }
            // Access mask 0 queries attributes without read/write access.
            let Some(h) = open_handle(&path, 0, 0) else {
                continue;
            };
            if attributes(&h) != Some(m.identity) {
                continue;
            }
            let Some(c) = caps(&h) else { continue };
            let d = HidDescriptor {
                identity: m.identity,
                usage_page: c.UsagePage,
                usage: c.Usage,
                product: string_attr(&h, HidD_GetProductString),
                serial: string_attr(&h, HidD_GetSerialNumberString),
                max_input_report_len: usize::from(c.InputReportByteLength),
                max_output_report_len: usize::from(c.OutputReportByteLength),
                interface_number: None,
                bus: None,
                path: HidPath::new(path)?,
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
        let h = open_handle(
            d.path.as_str(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_FLAG_OVERLAPPED,
        )
        .ok_or_else(|| os_err(HidOp::Open))?;
        if attributes(&h) != Some(d.identity) {
            return Err(HidError::NotFound);
        }
        let c = caps(&h).ok_or(HidError::NotFound)?;
        if c.UsagePage != d.usage_page || c.Usage != d.usage {
            return Err(HidError::NotFound);
        }
        Ok(Box::new(WinTransport {
            h,
            in_len: usize::from(c.InputReportByteLength),
            out_len: usize::from(c.OutputReportByteLength),
        }))
    }
}

struct WinTransport {
    h: Handle,
    in_len: usize,
    out_len: usize,
}

impl WinTransport {
    /// Runs one overlapped transfer, cancelling it if `timeout` elapses.
    fn transfer(
        &self,
        op: HidOp,
        buf: *mut u8,
        len: usize,
        timeout: Duration,
    ) -> Result<usize, HidError> {
        // SAFETY: manual-reset, unsignalled, unnamed event.
        let ev = unsafe { CreateEventW(null(), 1, 0, null()) };
        if ev.is_null() {
            return Err(os_err(op));
        }
        let ev = Handle(ev);
        let mut ov: OVERLAPPED = unsafe { core::mem::zeroed() };
        ov.hEvent = ev.0;
        let mut n = 0u32;
        // SAFETY: `buf` is valid for `len` bytes and, with `ov`, stays alive until the
        // operation completes: we always wait for completion after cancelling.
        let ok = unsafe {
            match op {
                HidOp::Write => WriteFile(self.h.0, buf, len as u32, null_mut(), &mut ov),
                _ => ReadFile(self.h.0, buf, len as u32, null_mut(), &mut ov),
            }
        };
        if ok == 0 {
            if unsafe { GetLastError() } != ERROR_IO_PENDING {
                return Err(os_err(op));
            }
            let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
            // SAFETY: valid event handle.
            if unsafe { WaitForSingleObject(ev.0, ms) } != WAIT_OBJECT_0 {
                // SAFETY: cancel then block until the kernel is done with buf/ov.
                unsafe {
                    CancelIoEx(self.h.0, &ov);
                    GetOverlappedResult(self.h.0, &ov, &mut n, 1);
                }
                return Err(HidError::Timeout);
            }
        }
        // SAFETY: operation has completed.
        if unsafe { GetOverlappedResult(self.h.0, &ov, &mut n, 0) } == 0 {
            return Err(os_err(op));
        }
        Ok(n as usize)
    }
}

impl HidTransport for WinTransport {
    fn write(&mut self, report: &[u8]) -> Result<usize, HidError> {
        // Windows requires exactly the device's output report length; zero-pad.
        let total = if self.out_len == 0 {
            report.len()
        } else {
            self.out_len
        };
        if report.len() > total {
            return Err(HidError::InvalidArgument(
                ArgumentError::ReportExceedsDevice,
            ));
        }
        let mut buf = vec![0u8; total];
        buf[..report.len()].copy_from_slice(report);
        let n = self.transfer(HidOp::Write, buf.as_mut_ptr(), total, WRITE_TIMEOUT)?;
        Ok(n.min(report.len()))
    }

    fn read(&mut self, out: &mut [u8], timeout: Duration) -> Result<usize, HidError> {
        // Windows requires a buffer of the full input report length.
        let total = if self.in_len == 0 {
            out.len()
        } else {
            self.in_len
        };
        if total > MAX_REPORT_LEN {
            return Err(HidError::Protocol("input report too large"));
        }
        let mut buf = vec![0u8; total];
        let n = self
            .transfer(HidOp::Read, buf.as_mut_ptr(), total, timeout)?
            .min(total);
        let n = n.min(out.len());
        out[..n].copy_from_slice(&buf[..n]);
        Ok(n)
    }
}
