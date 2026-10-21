//! Per-user Unix socket `$XDG_RUNTIME_DIR/freehm/app.sock`. The directory is created 0700,
//! so only the owning user can reach the socket; connections from other users are also
//! rejected by checking the peer's credentials.

use crate::app::EventSink;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

pub fn socket_path() -> PathBuf {
    crate::paths::runtime_dir().join("app.sock")
}

/// Create the private runtime directory (0700) or verify an existing one.
pub fn ensure_runtime_dir() -> Result<PathBuf, String> {
    let dir = crate::paths::runtime_dir();
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let md = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
    use std::os::unix::fs::MetadataExt;
    if !md.is_dir() || md.uid() != crate::paths::uid() {
        return Err(format!("{} is not a directory owned by this user", dir.display()));
    }
    if md.mode() & 0o077 != 0 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

fn peer_is_us(s: &UnixStream) -> bool {
    use std::os::fd::AsFd;
    match rustix::net::sockopt::socket_peercred(s.as_fd()) {
        Ok(c) => c.uid.as_raw() == crate::paths::uid(),
        Err(_) => false,
    }
}

pub fn start_server(sink: EventSink) -> Result<(), String> {
    ensure_runtime_dir()?;
    let path = socket_path();
    // The caller holds the single-instance lock, so an existing socket file is stale.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    std::thread::Builder::new()
        .name("ipc-listen".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if !peer_is_us(&stream) {
                    continue;
                }
                let sink = sink.clone();
                let _ = std::thread::Builder::new().name("ipc-client".into()).spawn(move || crate::ipc::serve(stream, &sink));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn remove_socket() {
    let _ = std::fs::remove_file(socket_path());
}

pub fn connect() -> std::io::Result<UnixStream> {
    UnixStream::connect(socket_path())
}
