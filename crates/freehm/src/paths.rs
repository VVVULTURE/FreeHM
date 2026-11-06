//! Per-user file locations.
//! - Windows: `%APPDATA%\FreeHM\config.json`, logs in `%LOCALAPPDATA%\FreeHM\logs`, downloaded
//!   updates in `%LOCALAPPDATA%\FreeHM\updates`.
//! - Linux (XDG): `$XDG_CONFIG_HOME/freehm/config.json`, logs in `$XDG_STATE_HOME/freehm`,
//!   IPC socket in `$XDG_RUNTIME_DIR/freehm`, downloaded updates in `$XDG_CACHE_HOME/freehm`.
//!
//! `FREEHM_CONFIG_DIR` overrides the configuration directory (used by tests and for
//! portable setups).

use std::path::PathBuf;

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg(windows)]
pub fn config_dir() -> PathBuf {
    if let Some(d) = env_dir("FREEHM_CONFIG_DIR") {
        return d;
    }
    env_dir("APPDATA").unwrap_or_else(|| PathBuf::from(".")).join("FreeHM")
}

#[cfg(windows)]
pub fn log_dir() -> PathBuf {
    if let Some(d) = env_dir("FREEHM_CONFIG_DIR") {
        return d.join("logs");
    }
    env_dir("LOCALAPPDATA").unwrap_or_else(|| PathBuf::from(".")).join("FreeHM").join("logs")
}

/// Downloaded updates, emptied whenever FreeHM starts.
#[cfg(windows)]
pub fn update_dir() -> PathBuf {
    if let Some(d) = env_dir("FREEHM_CONFIG_DIR") {
        return d.join("updates");
    }
    env_dir("LOCALAPPDATA").unwrap_or_else(|| PathBuf::from(".")).join("FreeHM").join("updates")
}

#[cfg(not(windows))]
pub fn update_dir() -> PathBuf {
    if let Some(d) = env_dir("FREEHM_CONFIG_DIR") {
        return d.join("updates");
    }
    env_dir("XDG_CACHE_HOME").unwrap_or_else(|| home().join(".cache")).join("freehm").join("updates")
}

#[cfg(not(windows))]
fn home() -> PathBuf {
    env_dir("HOME").unwrap_or_else(|| PathBuf::from("/tmp"))
}

#[cfg(not(windows))]
pub fn config_dir() -> PathBuf {
    if let Some(d) = env_dir("FREEHM_CONFIG_DIR") {
        return d;
    }
    env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config")).join("freehm")
}

#[cfg(not(windows))]
pub fn log_dir() -> PathBuf {
    if let Some(d) = env_dir("FREEHM_CONFIG_DIR") {
        return d.join("logs");
    }
    env_dir("XDG_STATE_HOME").unwrap_or_else(|| home().join(".local/state")).join("freehm")
}

/// Directory for the IPC socket: private (0700) and cleaned up by the session manager.
#[cfg(not(windows))]
pub fn runtime_dir() -> PathBuf {
    match env_dir("XDG_RUNTIME_DIR") {
        Some(d) => d.join("freehm"),
        // No runtime dir (rare: non-systemd session); fall back to a per-user dir in /tmp.
        None => PathBuf::from(format!("/tmp/freehm-{}", unsafe { libc_getuid() })),
    }
}

#[cfg(not(windows))]
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

#[cfg(not(windows))]
pub fn uid() -> u32 {
    unsafe { libc_getuid() }
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

pub fn log_file() -> PathBuf {
    log_dir().join("freehm.log")
}
