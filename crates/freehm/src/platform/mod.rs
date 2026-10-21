//! Platform dispatch. Each platform provides the same small surface used by `main`.

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use self::windows::{ipc_connect, set_dpi_awareness, settings_instance, single_instance};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::{ipc_connect, set_dpi_awareness, settings_instance, single_instance};

/// Run the background app. `open_settings`: show the settings window once after start-up
/// (first run).
pub fn run(cfg: freehm_core::config::Config, path: std::path::PathBuf, notes: Vec<String>, mock: bool, open_settings: bool) -> i32 {
    #[cfg(windows)]
    return self::windows::run(cfg, path, notes, mock, open_settings);
    #[cfg(target_os = "linux")]
    return self::linux::run(cfg, path, notes, mock, open_settings);
}
