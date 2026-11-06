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
        /// Also show the overlay for the next few seconds even where it would be hidden, so
        /// that appearance and position changes can be seen on screen (the settings window
        /// sends this while its Overlay page is open). `false` ends such a preview.
        #[serde(default)]
        show_overlay: bool,
    },
    /// Validate, apply immediately and persist a new configuration.
    ApplyConfig(Box<Config>),
    ToggleOverlay,
    SetOverlayVisible(bool),
    OpenSettings,
    /// Re-check helper/permission state (after the user ran the setup).
    RefreshPermissions,
    /// Look for a newer release on GitHub now.
    CheckForUpdate,
    /// The user chose to update: download, verify and install the release found.
    InstallUpdate,
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
    #[serde(default)]
    pub update: UpdateStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UpdatePhase {
    #[default]
    NotChecked,
    Checking,
    UpToDate,
    /// The check did not finish (no network, GitHub unavailable). Harmless.
    CheckFailed,
    Available,
    Downloading,
    /// Downloaded and verified; waiting until no game is active.
    WaitingForGame,
    /// The installer is running.
    Installing,
}

/// How this copy of FreeHM can be updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UpdateMethod {
    /// Installed some other way (.deb, .tar.gz, a build from source): the user updates it.
    #[default]
    Manual,
    /// Windows installer: asks first, then downloads and installs.
    Installer,
    /// Linux AppImage: replaces itself.
    AppImage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UpdateStatus {
    pub phase: UpdatePhase,
    pub method: UpdateMethod,
    /// The newer version found.
    pub latest: Option<String>,
    /// Its release page.
    pub page_url: Option<String>,
    /// 0–1 while downloading.
    pub progress: Option<f32>,
    /// What went wrong last, if anything.
    pub message: Option<String>,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MonitorInfo {
    /// The system's name for the output right now (`\\\\.\\DISPLAY2`, `DP-1`). On Windows
    /// the number can change between sessions.
    pub name: String,
    /// Identifies the monitor itself where the system can (empty otherwise).
    #[serde(default)]
    pub id: String,
    /// What the monitor calls itself (`VG27AQ`); empty when unknown.
    #[serde(default)]
    pub label: String,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

impl MonitorInfo {
    /// The name to show people.
    pub fn display_name(&self) -> &str {
        if self.label.is_empty() { &self.name } else { &self.label }
    }

    /// What to store for this monitor in the configuration.
    pub fn stored_id(&self) -> &str {
        if self.id.is_empty() { &self.name } else { &self.id }
    }

    /// Is this the monitor stored as `choice`? Configurations from before monitors had an
    /// id hold the system's name.
    pub fn matches(&self, choice: &str) -> bool {
        self.stored_id() == choice || self.name == choice
    }

    /// `VG27AQ — 2560×1440 (Primary)`. Identical monitors are told apart by the system's name.
    pub fn title(&self, all: &[MonitorInfo]) -> String {
        let name = self.display_name();
        let twins = all.iter().filter(|m| m.display_name() == name).count();
        let which = if twins > 1 { format!(" ({})", self.name.trim_start_matches(['\\', '.'])) } else { String::new() };
        format!("{name}{which} — {}×{}{}", self.width, self.height, if self.primary { " (Primary)" } else { "" })
    }
}

/// The monitor chosen as `choice`: by what was stored, or by its name when it came back on
/// another output (only when one connected monitor has that name).
pub fn find_monitor<'a>(monitors: &'a [MonitorInfo], choice: &str, label: &str) -> Option<&'a MonitorInfo> {
    monitors.iter().find(|m| m.matches(choice)).or_else(|| {
        let mut same = monitors.iter().filter(|m| !label.is_empty() && m.label == label);
        match (same.next(), same.next()) {
            (Some(m), None) => Some(m),
            _ => None,
        }
    })
}

/// Monitor name from an EDID block (descriptor tag 0xFC), used where the system gives none.
pub fn edid_monitor_name(edid: &[u8]) -> Option<String> {
    edid.get(54..126)?.as_chunks::<18>().0.iter().find(|d| d[..3] == [0, 0, 0] && d[3] == 0xFC).and_then(|d| {
        let text: String = d[5..].iter().take_while(|&&b| b != b'\n' && b != 0).map(|&b| b as char).collect();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    })
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
        for req in [
            Request::Hello { version: 1 },
            Request::GetStatus { live: true, show_overlay: true },
            Request::ApplyConfig(Box::default()),
            Request::ToggleOverlay,
            Request::Quit,
        ] {
            let mut buf = Vec::new();
            write_frame(&mut buf, &req, MAX_FRAME).unwrap();
            let back: Request = read_frame(&mut std::io::Cursor::new(buf), MAX_FRAME).unwrap();
            assert_eq!(back, req);
        }
    }

    #[test]
    fn monitors_are_found_by_id_or_by_their_old_system_name() {
        let m = MonitorInfo {
            name: r"\\.\DISPLAY2".into(),
            id: "path".into(),
            label: "VG27AQ".into(),
            primary: false,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            scale: 1.0,
        };
        assert!(m.matches("path"));
        assert!(m.matches(r"\\.\DISPLAY2"), "stored by an older version");
        assert!(!m.matches(r"\\.\DISPLAY1"));
        assert_eq!(m.display_name(), "VG27AQ");
        let plain = MonitorInfo { id: String::new(), label: String::new(), ..m };
        assert_eq!((plain.display_name(), plain.stored_id()), (r"\\.\DISPLAY2", r"\\.\DISPLAY2"));
    }

    fn monitor(name: &str, id: &str, label: &str, primary: bool) -> MonitorInfo {
        MonitorInfo { name: name.into(), id: id.into(), label: label.into(), primary, width: 2560, height: 1440, scale: 1.0, ..Default::default() }
    }

    #[test]
    fn a_monitor_stays_chosen_when_windows_renumbers_it() {
        let before = [monitor(r"\\.\DISPLAY1", "a", "VG27AQ", true), monitor(r"\\.\DISPLAY2", "b", "LG Ultra", false)];
        let after = [monitor(r"\\.\DISPLAY2", "a", "VG27AQ", true), monitor(r"\\.\DISPLAY3", "b", "LG Ultra", false)];
        assert_eq!(find_monitor(&before, "b", "LG Ultra").unwrap().label, "LG Ultra");
        assert_eq!(find_monitor(&after, "b", "LG Ultra").unwrap().name, r"\\.\DISPLAY3");
    }

    #[test]
    fn a_monitor_on_another_output_is_found_by_its_name_unless_that_is_ambiguous() {
        let moved = [monitor("DP-1", "", "VG27AQ", true), monitor("HDMI-1", "", "LG Ultra", false)];
        assert_eq!(find_monitor(&moved, "DP-2", "LG Ultra").unwrap().name, "HDMI-1");
        assert!(find_monitor(&moved, "DP-2", "").is_none(), "no name known");
        assert!(find_monitor(&moved, "DP-2", "Other").is_none(), "not connected");
        let twins = [monitor("DP-1", "", "VG27AQ", true), monitor("HDMI-1", "", "VG27AQ", false)];
        assert!(find_monitor(&twins, "DP-2", "VG27AQ").is_none(), "either could be it");
    }

    #[test]
    fn monitor_titles_show_the_name_and_tell_twins_apart() {
        let a = monitor(r"\\.\DISPLAY1", "a", "VG27AQ", true);
        let b = monitor(r"\\.\DISPLAY2", "b", "LG Ultra", false);
        assert_eq!(a.title(&[a.clone(), b.clone()]), "VG27AQ — 2560×1440 (Primary)");
        assert_eq!(b.title(&[a.clone(), b.clone()]), "LG Ultra — 2560×1440");
        let twin = monitor(r"\\.\DISPLAY2", "c", "VG27AQ", false);
        assert_eq!(twin.title(&[a.clone(), twin.clone()]), "VG27AQ (DISPLAY2) — 2560×1440");
    }

    #[test]
    fn a_status_from_an_older_version_still_parses() {
        let old = r#"{"name":"DP-1","primary":true,"x":0,"y":0,"width":1,"height":1,"scale":1.0}"#;
        let m: MonitorInfo = serde_json::from_str(old).unwrap();
        assert_eq!(m.display_name(), "DP-1");
    }

    #[test]
    fn reads_the_monitor_name_from_edid() {
        let mut edid = vec![0u8; 128];
        edid[54 + 18..54 + 18 + 18].copy_from_slice(b"\0\0\0\xFC\0VG27AQ\n      ");
        assert_eq!(edid_monitor_name(&edid).as_deref(), Some("VG27AQ"));
        edid[54 + 18 + 3] = 0xFE;
        assert_eq!(edid_monitor_name(&edid), None, "another descriptor");
        assert_eq!(edid_monitor_name(&[0; 10]), None, "too short");
    }

    #[test]
    fn status_request_without_show_overlay_still_parses() {
        // Scripts and older settings windows send only `live`.
        let r: Request = serde_json::from_str(r#"{"GetStatus":{"live":true}}"#).unwrap();
        assert_eq!(r, Request::GetStatus { live: true, show_overlay: false });
    }
}
