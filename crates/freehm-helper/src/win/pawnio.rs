//! Minimal binding to PawnIOLib (LGPL-2.1, installed with the PawnIO driver).
//!
//! The DLL is loaded from the PawnIO installation directory by absolute path with a
//! restricted search path, so a DLL planted elsewhere can never be loaded into this
//! SYSTEM process.

use std::ffi::CStr;
use windows::Win32::Foundation::{FreeLibrary, HANDLE, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::core::{HRESULT, PCSTR, PCWSTR};

type OpenFn = unsafe extern "system" fn(*mut HANDLE) -> HRESULT;
type LoadFn = unsafe extern "system" fn(HANDLE, *const u8, usize) -> HRESULT;
type ExecFn = unsafe extern "system" fn(HANDLE, PCSTR, *const u64, usize, *mut u64, usize, *mut usize) -> HRESULT;
type CloseFn = unsafe extern "system" fn(HANDLE) -> HRESULT;

pub struct PawnIo {
    lib: HMODULE,
    handle: HANDLE,
    exec: ExecFn,
    close: CloseFn,
}

// The executor handle is a kernel handle; calls are serialised by the owner's mutex.
unsafe impl Send for PawnIo {}

fn dll_path() -> std::path::PathBuf {
    let pf = std::env::var_os("ProgramW6432").or_else(|| std::env::var_os("ProgramFiles")).unwrap_or_else(|| r"C:\Program Files".into());
    std::path::PathBuf::from(pf).join("PawnIO").join("PawnIOLib.dll")
}

fn hr(r: HRESULT, what: &str) -> Result<(), String> {
    if r.is_ok() { Ok(()) } else { Err(format!("{what} failed (0x{:08X}: {})", r.0 as u32, r.message())) }
}

impl PawnIo {
    /// Open an executor and load `module` (a signed PawnIO module blob).
    pub fn open(module: &[u8]) -> Result<PawnIo, String> {
        let path = dll_path();
        if !path.exists() {
            return Err("the PawnIO driver is not installed (winget install namazso.PawnIO)".into());
        }
        let wpath: Vec<u16> = path.as_os_str().encode_wide_nul();
        unsafe {
            let lib = LoadLibraryExW(PCWSTR(wpath.as_ptr()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32)
                .map_err(|e| format!("cannot load {}: {e}", path.display()))?;
            let get = |name: &CStr| GetProcAddress(lib, PCSTR(name.as_ptr().cast()));
            let (Some(open), Some(load), Some(exec), Some(close)) = (get(c"pawnio_open"), get(c"pawnio_load"), get(c"pawnio_execute"), get(c"pawnio_close"))
            else {
                let _ = FreeLibrary(lib);
                return Err("PawnIOLib.dll does not export the expected functions (unsupported PawnIO version)".into());
            };
            let open: OpenFn = std::mem::transmute(open);
            let load: LoadFn = std::mem::transmute(load);
            let exec: ExecFn = std::mem::transmute(exec);
            let close: CloseFn = std::mem::transmute(close);
            let mut handle = HANDLE::default();
            if let Err(e) = hr(open(&mut handle), "pawnio_open") {
                let _ = FreeLibrary(lib);
                return Err(e);
            }
            let pio = PawnIo { lib, handle, exec, close };
            hr(load(handle, module.as_ptr(), module.len()), "loading the PawnIO module")?;
            Ok(pio)
        }
    }

    /// Run module function `name`; returns the number of output values written.
    pub fn call(&self, name: &CStr, input: &[u64], out: &mut [u64]) -> Result<usize, String> {
        let mut written = 0usize;
        let r = unsafe { (self.exec)(self.handle, PCSTR(name.as_ptr().cast()), input.as_ptr(), input.len(), out.as_mut_ptr(), out.len(), &mut written) };
        hr(r, &format!("PawnIO {}", name.to_string_lossy()))?;
        Ok(written)
    }

    pub fn read1(&self, name: &CStr, arg: u64) -> Result<u64, String> {
        let mut out = [0u64; 1];
        match self.call(name, &[arg], &mut out)? {
            0 => Err(format!("PawnIO {} returned no value", name.to_string_lossy())),
            _ => Ok(out[0]),
        }
    }
}

impl Drop for PawnIo {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.close)(self.handle);
            let _ = FreeLibrary(self.lib);
        }
    }
}

trait EncodeWideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl EncodeWideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}
