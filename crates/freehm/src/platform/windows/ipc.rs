//! Per-user named pipe for app ⇄ settings/CLI communication.
//!
//! The pipe name contains the user's SID and its DACL grants access only to that user and
//! SYSTEM (network logons are denied by default for local pipes created with an explicit
//! DACL that does not list them). `FILE_FLAG_FIRST_PIPE_INSTANCE` makes creation fail if
//! another process already owns the name, so the app never shares a pipe with an impostor.

use super::util::{current_user_sid, wide};
use crate::app::EventSink;
use std::fs::File;
use std::os::windows::io::FromRawHandle;
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows::core::PCWSTR;

pub fn pipe_name() -> String {
    let sid = current_user_sid().unwrap_or_else(|| std::env::var("USERNAME").unwrap_or_default());
    format!(r"\\.\pipe\FreeHM.App.{sid}")
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// The descriptor is an immutable LocalAlloc'd block; moving it to the listener thread is safe.
unsafe impl Send for SecurityDescriptor {}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

fn user_only_sd() -> Result<SecurityDescriptor, String> {
    let sid = current_user_sid().ok_or("cannot determine current user SID")?;
    // Protected DACL: full access for this user and LocalSystem only.
    let sddl = wide(&format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"));
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None) }.map_err(|e| e.to_string())?;
    Ok(SecurityDescriptor(sd))
}

fn create_instance(name: &[u16], sd: &SecurityDescriptor, first: bool) -> Result<File, String> {
    let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0.0, bInheritHandle: false.into() };
    let mut open = PIPE_ACCESS_DUPLEX;
    if first {
        open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            open,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            8,
            64 * 1024,
            64 * 1024,
            0,
            Some(&sa),
        )
    };
    if h.is_invalid() {
        return Err(format!("CreateNamedPipe failed: {}", windows::core::Error::from_thread()));
    }
    Ok(unsafe { File::from_raw_handle(h.0) })
}

/// Start listening. Fails if the pipe already exists (another instance or an impostor).
pub fn start_server(sink: EventSink) -> Result<(), String> {
    let name = wide(&pipe_name());
    let sd = user_only_sd()?;
    let first = create_instance(&name, &sd, true)?;
    std::thread::Builder::new()
        .name("ipc-listen".into())
        .spawn(move || {
            let mut next = Some(first);
            loop {
                let pipe = match next.take() {
                    Some(p) => p,
                    None => match create_instance(&name, &sd, false) {
                        Ok(p) => p,
                        Err(e) => {
                            log::error!("{e}");
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            continue;
                        }
                    },
                };
                let h = windows::Win32::Foundation::HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&pipe));
                let connected = match unsafe { ConnectNamedPipe(h, None) } {
                    Ok(()) => true,
                    Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
                };
                if !connected {
                    continue;
                }
                let sink = sink.clone();
                let _ = std::thread::Builder::new().name("ipc-client".into()).spawn(move || crate::ipc::serve(pipe, &sink));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn connect() -> std::io::Result<File> {
    let name = pipe_name();
    let mut last = None;
    for _ in 0..20 {
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            Ok(f) => return Ok(f),
            // ERROR_PIPE_BUSY (231): all instances in use, try again shortly.
            Err(e) if e.raw_os_error() == Some(231) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("pipe busy")))
}
