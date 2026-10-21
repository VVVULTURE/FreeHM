//! Start with Windows via the per-user `Run` key (no elevation; the documented mechanism
//! that Task Manager's "Startup apps" page manages). If the user disabled FreeHM in Task
//! Manager, Windows records that under `StartupApproved\Run`, and we report it rather than
//! silently overriding the user's choice.

use super::util::wide;
use freehm_core::ipc::AutostartStatus;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::*;
use windows::core::PCWSTR;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const VALUE: &str = "FreeHM";

fn command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(format!("\"{}\" --background", exe.display()))
}

fn disabled_in_task_manager() -> bool {
    let (k, v) = (wide(APPROVED), wide(VALUE));
    let mut data = [0u8; 12];
    let mut size = data.len() as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), PCWSTR(v.as_ptr()), RRF_RT_REG_BINARY, None, Some(data.as_mut_ptr().cast()), Some(&mut size))
    };
    // First byte 0x02 = enabled, 0x03 = disabled by the user.
    r == ERROR_SUCCESS && size >= 1 && data[0] & 1 == 1
}

pub fn status() -> AutostartStatus {
    let current = super::util::reg_string(HKEY_CURRENT_USER, RUN, VALUE);
    let expected = command().ok();
    let enabled = current.is_some();
    let error = if enabled && disabled_in_task_manager() {
        Some("FreeHM is disabled in Task Manager ▸ Startup apps; re-enable it there to start with Windows".into())
    } else if enabled && current != expected {
        Some("the startup entry points to a different FreeHM location; toggle the setting to repair it".into())
    } else {
        None
    };
    AutostartStatus { enabled, mechanism: r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run".into(), error }
}

pub fn set(on: bool) -> Result<(), String> {
    let mut key = HKEY::default();
    let k = wide(RUN);
    let r =
        unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None) };
    if r != ERROR_SUCCESS {
        return Err(format!("cannot open Run key: {r:?}"));
    }
    let v = wide(VALUE);
    let res = if on {
        let cmd = wide(&command()?);
        let bytes = unsafe { std::slice::from_raw_parts(cmd.as_ptr().cast::<u8>(), cmd.len() * 2) };
        unsafe { RegSetValueExW(key, PCWSTR(v.as_ptr()), None, REG_SZ, Some(bytes)) }
    } else {
        let r = unsafe { RegDeleteValueW(key, PCWSTR(v.as_ptr())) };
        if r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND { ERROR_SUCCESS } else { r }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if res == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry write failed: {res:?}")) }
}
