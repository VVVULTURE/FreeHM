//! Security descriptors from SDDL strings.

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::core::PCWSTR;

pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// An immutable LocalAlloc'd block; safe to move between threads.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> Result<SecurityDescriptor, String> {
        let w: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(w.as_ptr()), SDDL_REVISION_1, &mut sd, None) }
            .map_err(|e| format!("invalid security descriptor: {e}"))?;
        Ok(SecurityDescriptor(sd))
    }

    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: self.0.0, bInheritHandle: false.into() }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

/// Helper pipe: SYSTEM and Administrators full control; interactive users may only
/// read/write data on an existing instance (FILE_READ_DATA | FILE_WRITE_DATA |
/// FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE = 0x120083). Notably they lack
/// FILE_CREATE_PIPE_INSTANCE, so no user process can add a rogue server instance.
pub const HELPER_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x120083;;;IU)";

/// Log directory: SYSTEM/Administrators full control, users read.
pub const LOG_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)";
