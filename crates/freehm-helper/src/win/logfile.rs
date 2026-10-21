//! Logging for the service (a size-capped file in a directory only administrators can
//! write) and output for the install/uninstall commands.

use super::sd::{LOG_DIR_SDDL, SecurityDescriptor};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::{IsWellKnownSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, WinBuiltinAdministratorsSid, WinLocalSystemSid};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::core::PCWSTR;

const MAX_BYTES: u64 = 512 * 1024;

pub fn log_dir() -> PathBuf {
    let pd = std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
    PathBuf::from(pd).join("FreeHM")
}

fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// ProgramData is writable by every user, so a pre-created folder could be a trap for a
/// SYSTEM process. Create it with a restrictive ACL, or accept an existing one only if it
/// is owned by SYSTEM or Administrators.
fn secure_dir(dir: &Path) -> bool {
    let w = wide(dir);
    let Ok(sd) = SecurityDescriptor::from_sddl(LOG_DIR_SDDL) else { return false };
    let sa = sd.attributes();
    unsafe {
        if CreateDirectoryW(PCWSTR(w.as_ptr()), Some(&sa)).is_ok() {
            return true;
        }
        if windows::Win32::Foundation::GetLastError() != ERROR_ALREADY_EXISTS {
            return false;
        }
        let mut owner = PSID::default();
        let mut psd = PSECURITY_DESCRIPTOR::default();
        let r = GetNamedSecurityInfoW(PCWSTR(w.as_ptr()), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, &mut psd);
        let ok = r.is_ok() && (IsWellKnownSid(owner, WinLocalSystemSid).as_bool() || IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool());
        if !psd.0.is_null() {
            let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(psd.0)));
        }
        ok
    }
}

struct FileLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let _g = self.lock.lock();
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let _ = writeln!(f, "{secs} {:5} {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

pub fn init_service_log() {
    let dir = log_dir();
    if !secure_dir(&dir) {
        return; // run without a log file rather than write into an untrusted folder
    }
    let logger = FileLog { path: dir.join("helper.log"), lock: Mutex::new(()) };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// Output of `install`/`uninstall`: stdout, plus an optional file the (unelevated)
/// settings window reads afterwards. The file must be `freehm-helper-*.log` directly in
/// the temporary directory, so an elevated run can't be pointed at arbitrary files.
pub struct CliOutput {
    file: Option<PathBuf>,
}

impl CliOutput {
    pub fn new(path: Option<PathBuf>) -> CliOutput {
        let file = path.filter(|p| {
            let name_ok = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("freehm-helper-") && n.ends_with(".log"));
            let dir_ok = match (p.parent().and_then(|d| d.canonicalize().ok()), std::env::temp_dir().canonicalize().ok()) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            };
            name_ok && dir_ok && std::fs::symlink_metadata(p).map(|m| m.file_type().is_file()).unwrap_or(true)
        });
        if let Some(f) = &file {
            let _ = std::fs::remove_file(f);
        }
        CliOutput { file }
    }

    pub fn line(&self, s: &str) {
        println!("{s}");
        if let Some(f) = &self.file
            && let Ok(mut h) = OpenOptions::new().create(true).append(true).open(f)
        {
            let _ = writeln!(h, "{s}");
        }
    }
}
