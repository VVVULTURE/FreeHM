//! Protocol between the background app and its clients (the settings window process and
//! command-line invocations such as `freehm --toggle`). The endpoint is per-user: a named
//! pipe whose DACL grants only the current user (Windows) or a socket in the 0700
//! `$XDG_RUNTIME_DIR/freehm` directory (Linux).

use crate::config::Config;
use crate::metrics::Snapshot;
use serde::{Deserialize, Serialize};

pub const MAX_FRAME: usize = 4 * 1024 * 1024;
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Hello {
        version: u32,
    },
    /// Full status; `live: true` also asks for full-rate polling of *all* metrics for the
    /// next few seconds (the settings window refreshes this while it is open).
    GetStatus {
        live: bool,
    },
    /// Validate, apply immediately and persist a new configuration.
    ApplyConfig(Box<Config>),
    ToggleOverlay,
    SetOverlayVisible(bool),
    OpenSettings,
    /// Re-check helper/permission state (after the user ran the setup).
    RefreshPermissions,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Hello {
        version: u32,
        app_version: String,
    },
    Status(Box<Status>),
    /// The configuration was not applied; one message per problem.
    Rejected(Vec<String>),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Status {
    pub app_version: String,
    pub platform: String,
    pub config: Config,
    pub config_path: String,
    pub log_path: String,
    pub snapshot: Snapshot,
    pub overlay: OverlayStatus,
    pub hotkey: HotkeyStatus,
    pub game: GameStatus,
    pub helper: HelperStatus,
    pub autostart: AutostartStatus,
    pub monitors: Vec<MonitorInfo>,
    /// Problems found while loading the configuration or fonts.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OverlayStatus {
    pub visible: bool,
    /// Why it is (not) visible right now, e.g. "hidden: no game detected".
    pub reason: String,
    /// Backend in use, e.g. "Win32 layered window", "wlr-layer-shell", "X11 override-redirect".
    pub backend: String,
    /// Set when the overlay cannot be shown in the current session/display mode.
    pub unsupported: Option<String>,
    /// Non-fatal warning, e.g. exclusive fullscreen detected.
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HotkeyStatus {
    pub shortcut: String,
    pub registered: bool,
    pub error: Option<String>,
    /// How the shortcut is implemented ("RegisterHotKey", "X11 key grab", "XDG portal", ...).
    pub mechanism: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GameStatus {
    pub detection_enabled: bool,
    pub active: Option<GameInfo>,
    /// The foreground process and why it was (not) classified as a game — for fixing
    /// false positives/negatives from the settings window.
    pub foreground: Option<GameInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameInfo {
    pub pid: u32,
    pub name: String,
    pub path: String,
    pub is_game: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HelperStatus {
    /// Whether this platform uses a helper at all.
    pub applicable: bool,
    pub installed: bool,
    pub running: bool,
    pub connected: bool,
    pub message: String,
    pub pawnio_installed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AutostartStatus {
    pub enabled: bool,
    pub mechanism: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub name: String,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{read_frame, write_frame};

    #[test]
    fn status_round_trips_through_wire() {
        let st = Status { app_version: "0.1.0".into(), ..Default::default() };
        let mut buf = Vec::new();
        write_frame(&mut buf, &Response::Status(Box::new(st.clone())), MAX_FRAME).unwrap();
        let back: Response = read_frame(&mut std::io::Cursor::new(buf), MAX_FRAME).unwrap();
        assert_eq!(back, Response::Status(Box::new(st)));
    }

    #[test]
    fn requests_round_trip() {
        for req in
            [Request::Hello { version: 1 }, Request::GetStatus { live: true }, Request::ApplyConfig(Box::default()), Request::ToggleOverlay, Request::Quit]
        {
            let mut buf = Vec::new();
            write_frame(&mut buf, &req, MAX_FRAME).unwrap();
            let back: Request = read_frame(&mut std::io::Cursor::new(buf), MAX_FRAME).unwrap();
            assert_eq!(back, req);
        }
    }
}
