//! Small Win32 helpers: wide strings, registry reads, current-user SID.

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Registry::{HKEY, KEY_READ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// Read a REG_SZ / REG_EXPAND_SZ value.
pub fn reg_string(root: HKEY, path: &str, name: &str) -> Option<String> {
    let (p, n) = (wide(path), wide(name));
    let mut size: u32 = 0;
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    unsafe {
        if RegGetValueW(root, PCWSTR(p.as_ptr()), PCWSTR(n.as_ptr()), flags, None, None, Some(&mut size)) != ERROR_SUCCESS || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut size2 = (buf.len() * 2) as u32;
        if RegGetValueW(root, PCWSTR(p.as_ptr()), PCWSTR(n.as_ptr()), flags, None, Some(buf.as_mut_ptr().cast()), Some(&mut size2)) != ERROR_SUCCESS {
            return None;
        }
        Some(from_wide(&buf))
    }
}

pub fn reg_key_exists(root: HKEY, path: &str) -> bool {
    let p = wide(path);
    let mut k = HKEY::default();
    unsafe {
        if RegOpenKeyExW(root, PCWSTR(p.as_ptr()), None, KEY_READ, &mut k) == ERROR_SUCCESS {
            let _ = RegCloseKey(k);
            true
        } else {
            false
        }
    }
}

/// Names of the direct subkeys of `path`.
pub fn reg_subkeys(root: HKEY, path: &str) -> Vec<String> {
    let p = wide(path);
    let mut k = HKEY::default();
    let mut out = Vec::new();
    unsafe {
        if RegOpenKeyExW(root, PCWSTR(p.as_ptr()), None, KEY_READ, &mut k) != ERROR_SUCCESS {
            return out;
        }
        for i in 0..4096u32 {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(k, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        let _ = RegCloseKey(k);
    }
    out
}

/// String SID of the user running this process (e.g. `S-1-5-21-...`).
pub fn current_user_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        ok.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s).ok()?;
        let out = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        out
    }
}

/// RAII wrapper closing a HANDLE.
pub struct OwnedHandle(pub HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

unsafe impl Send for OwnedHandle {}
