//! the few NDI SDK calls a sender needs, resolved with dlopen so the
//! binary starts and runs without libndi until NDI out is switched on.
//! layouts follow Processing.NDI.structs.h and Processing.NDI.Send.h of
//! SDK 6.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::OnceLock;

use anyhow::{bail, Result};

#[repr(C)]
pub struct SendCreate {
    pub ndi_name: *const c_char,
    pub groups: *const c_char,
    pub clock_video: bool,
    pub clock_audio: bool,
}

#[repr(C)]
pub struct VideoFrameV2 {
    pub xres: c_int,
    pub yres: c_int,
    pub fourcc: u32,
    pub frame_rate_n: c_int,
    pub frame_rate_d: c_int,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: c_int,
    pub timecode: i64,
    pub data: *const u8,
    pub line_stride_in_bytes: c_int,
    pub metadata: *const c_char,
    pub timestamp: i64,
}

pub const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

pub const FOURCC_UYVY: u32 = fourcc(b"UYVY");
pub const FRAME_FORMAT_PROGRESSIVE: c_int = 1;
pub const TIMECODE_SYNTHESIZE: i64 = i64::MAX;

pub type SendInstance = *mut c_void;

pub struct Lib {
    _handle: *mut c_void,
    pub version: String,
    pub send_create: unsafe extern "C" fn(*const SendCreate) -> SendInstance,
    pub send_destroy: unsafe extern "C" fn(SendInstance),
    pub send_video_async_v2: unsafe extern "C" fn(SendInstance, *const VideoFrameV2),
    pub send_get_no_connections: unsafe extern "C" fn(SendInstance, u32) -> c_int,
}

// the handle is only kept so the library is never unloaded, and the SDK's
// functions are documented as callable from any thread
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

/// $NDI_RUNTIME_DIR_V6 first, the SDK's documented override, then the
/// loader's own search path, which the nix wrapper and dev shell extend
fn candidates() -> Vec<CString> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::var_os("NDI_RUNTIME_DIR_V6") {
        let path = std::path::Path::new(&dir).join("libndi.so.6");
        if let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) {
            out.push(c);
        }
    }
    out.push(c"libndi.so.6".to_owned());
    out.push(c"libndi.so".to_owned());
    out
}

unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &CStr) -> Result<T> {
    let ptr = libc::dlsym(handle, name.as_ptr());
    if ptr.is_null() {
        bail!("libndi has no {}", name.to_string_lossy());
    }
    Ok(std::mem::transmute_copy(&ptr))
}

fn load() -> Result<Lib> {
    let mut tried = Vec::new();
    for path in candidates() {
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            tried.push(path.to_string_lossy().into_owned());
            continue;
        }
        unsafe {
            let initialize: unsafe extern "C" fn() -> bool = symbol(handle, c"NDIlib_initialize")?;
            let version: unsafe extern "C" fn() -> *const c_char =
                symbol(handle, c"NDIlib_version")?;
            if !initialize() {
                bail!("NDIlib_initialize failed: this cpu lacks what the sdk needs");
            }
            let version = CStr::from_ptr(version()).to_string_lossy().into_owned();
            return Ok(Lib {
                _handle: handle,
                version,
                send_create: symbol(handle, c"NDIlib_send_create")?,
                send_destroy: symbol(handle, c"NDIlib_send_destroy")?,
                send_video_async_v2: symbol(handle, c"NDIlib_send_send_video_async_v2")?,
                send_get_no_connections: symbol(handle, c"NDIlib_send_get_no_connections")?,
            });
        }
    }
    bail!("cannot load libndi, tried {}", tried.join(", "))
}

/// loads and initialises the SDK once per process; NDIlib_destroy is
/// never called, the process exit tears it down
pub fn lib() -> Result<&'static Lib> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(|| load().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uyvy_fourcc_matches_the_sdk() {
        // NDI_LIB_FOURCC('U','Y','V','Y') = 'U' | 'Y' << 8 | 'V' << 16 | 'Y' << 24
        assert_eq!(FOURCC_UYVY, 0x5956_5955);
    }

    #[test]
    fn video_frame_layout_matches_the_header() {
        assert_eq!(std::mem::size_of::<VideoFrameV2>(), 72);
        assert_eq!(std::mem::offset_of!(VideoFrameV2, timecode), 32);
        assert_eq!(std::mem::offset_of!(VideoFrameV2, data), 40);
        assert_eq!(std::mem::offset_of!(VideoFrameV2, metadata), 56);
        assert_eq!(std::mem::size_of::<SendCreate>(), 24);
    }
}
