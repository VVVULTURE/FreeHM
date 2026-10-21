//! Linux game-detection facts, read from `/proc` without opening any process for memory
//! access: `stat` (name, parent), the `exe` link, `cmdline` (argv[0] carries the Windows
//! path of Wine/Proton games) and `environ` (Steam sets `SteamGameId` for games it starts).
//! `exe`/`environ` are readable only for the user's own processes, which is where games run.
//!
//! The foreground window comes from `_NET_ACTIVE_WINDOW` on X11 and from the compositor's
//! own IPC on Hyprland and Sway. Other Wayland compositors do not expose it; detection then
//! falls back to scanning processes (see `detect.rs`).

use super::x11::ActiveWindow;
use crate::detect::{Foreground, GameProbe};
use freehm_core::gamedetect::{LibraryRoot, ProcessInfo, file_name, parse_steam_libraryfolders};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub enum ForegroundSource {
    X11(ActiveWindow),
    Hyprland(PathBuf),
    Sway(PathBuf),
    None,
}

impl ForegroundSource {
    /// Pick the source for this session. `x11` is the overlay's connection when the session
    /// is X11 (on Wayland, XWayland's active window does not reflect native windows).
    pub fn detect(x11: Option<ActiveWindow>, wayland_session: bool) -> ForegroundSource {
        if let Some(sig) = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") {
            let rt = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
            let p = rt.join("hypr").join(&sig).join(".socket.sock");
            let legacy = PathBuf::from("/tmp/hypr").join(&sig).join(".socket.sock");
            return ForegroundSource::Hyprland(if p.exists() { p } else { legacy });
        }
        if let Some(s) = std::env::var_os("SWAYSOCK") {
            return ForegroundSource::Sway(PathBuf::from(s));
        }
        match x11 {
            Some(a) if !wayland_session => ForegroundSource::X11(a),
            _ => ForegroundSource::None,
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            ForegroundSource::X11(_) => "X11 _NET_ACTIVE_WINDOW",
            ForegroundSource::Hyprland(_) => "Hyprland IPC",
            ForegroundSource::Sway(_) => "Sway IPC",
            ForegroundSource::None => "none (this compositor does not expose the focused window; processes are scanned instead)",
        }
    }
}

fn ipc_stream(p: &Path) -> Option<UnixStream> {
    let s = UnixStream::connect(p).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
    Some(s)
}

fn hyprland_active(sock: &Path) -> Result<Option<Foreground>, ()> {
    let mut s = ipc_stream(sock).ok_or(())?;
    s.write_all(b"j/activewindow").map_err(|_| ())?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).map_err(|_| ())?;
    parse_hyprland_active(&buf)
}

pub fn parse_hyprland_active(json: &[u8]) -> Result<Option<Foreground>, ()> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|_| ())?;
    let Some(pid) = v.get("pid").and_then(|p| p.as_i64()).filter(|&p| p > 0) else { return Ok(None) };
    // Older releases: bool; 0.42+: 0 none, 1 maximised, 2 fullscreen, 3 both.
    let fullscreen = match v.get("fullscreen") {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(n) => n.as_i64().is_some_and(|n| n >= 2),
        None => false,
    };
    Ok(Some(Foreground { pid: pid as u32, fullscreen }))
}

fn sway_active(sock: &Path) -> Result<Option<Foreground>, ()> {
    let mut s = ipc_stream(sock).ok_or(())?;
    // i3-ipc: magic, payload length, message type 4 = GET_TREE.
    let mut msg = b"i3-ipc".to_vec();
    msg.extend_from_slice(&0u32.to_ne_bytes());
    msg.extend_from_slice(&4u32.to_ne_bytes());
    s.write_all(&msg).map_err(|_| ())?;
    let mut hdr = [0u8; 14];
    s.read_exact(&mut hdr).map_err(|_| ())?;
    if &hdr[..6] != b"i3-ipc" {
        return Err(());
    }
    let len = u32::from_ne_bytes(hdr[6..10].try_into().unwrap()) as usize;
    if len > 64 << 20 {
        return Err(());
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).map_err(|_| ())?;
    parse_sway_tree(&body)
}

pub fn parse_sway_tree(json: &[u8]) -> Result<Option<Foreground>, ()> {
    fn find(n: &serde_json::Value) -> Option<&serde_json::Value> {
        if n.get("focused").and_then(|f| f.as_bool()) == Some(true) {
            return Some(n);
        }
        for key in ["nodes", "floating_nodes"] {
            if let Some(arr) = n.get(key).and_then(|a| a.as_array()) {
                for c in arr {
                    if let Some(f) = find(c) {
                        return Some(f);
                    }
                }
            }
        }
        None
    }
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|_| ())?;
    let Some(n) = find(&v) else { return Ok(None) };
    let Some(pid) = n.get("pid").and_then(|p| p.as_i64()).filter(|&p| p > 0) else { return Ok(None) };
    let fullscreen = n.get("fullscreen_mode").and_then(|m| m.as_i64()).unwrap_or(0) > 0;
    Ok(Some(Foreground { pid: pid as u32, fullscreen }))
}

/// Per-process facts that do not change during the process's life, keyed by pid and start
/// time (so a recycled pid is not mistaken for the old process).
#[derive(Clone)]
struct Cached {
    start: u64,
    name: String,
    paths: Vec<String>,
    steam_game: bool,
}

pub struct LinuxProbe {
    fg: ForegroundSource,
    cache: HashMap<u32, Cached>,
}

impl LinuxProbe {
    pub fn new(fg: ForegroundSource) -> LinuxProbe {
        LinuxProbe { fg, cache: HashMap::new() }
    }
}

/// `(comm, ppid, starttime)` from `/proc/<pid>/stat`. `comm` may contain spaces and
/// parentheses, so split at the last `)`.
pub fn parse_stat(s: &str) -> Option<(String, u32, u64)> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = s.get(close + 1..)?.split_whitespace().collect();
    // rest[0] = state, rest[1] = ppid, … rest[19] = starttime (field 22).
    let ppid = rest.get(1)?.parse().ok()?;
    let start = rest.get(19)?.parse().ok()?;
    Some((comm, ppid, start))
}

/// argv[0] when it is a Windows executable path (Wine/Proton), e.g. `Z:\\…\\Game.exe`.
pub fn wine_argv0(cmdline: &[u8]) -> Option<String> {
    let a0 = cmdline.split(|&b| b == 0).next()?;
    let a0 = String::from_utf8_lossy(a0).into_owned();
    a0.to_lowercase().ends_with(".exe").then_some(a0)
}

/// Steam starts games with `SteamGameId`/`SteamAppId` set to a non-zero app id.
pub fn steam_env(environ: &[u8]) -> bool {
    environ.split(|&b| b == 0).any(|kv| {
        let kv = String::from_utf8_lossy(kv);
        let Some((k, v)) = kv.split_once('=') else { return false };
        (k == "SteamGameId" || k == "SteamAppId") && v.parse::<u64>().is_ok_and(|n| n > 0)
    })
}

/// Convert a Wine path to a Unix path when it is on the `Z:` (root) drive.
fn wine_to_unix(p: &str) -> Option<String> {
    let rest = p.strip_prefix("Z:").or_else(|| p.strip_prefix("z:"))?;
    Some(rest.replace('\\', "/"))
}

impl LinuxProbe {
    fn read(&mut self, pid: u32) -> Option<ProcessInfo> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (comm, ppid, start) = parse_stat(&stat)?;
        let c = match self.cache.get(&pid) {
            Some(c) if c.start == start => c.clone(),
            _ => {
                let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok().map(|p| p.to_string_lossy().into_owned());
                let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                let wine = wine_argv0(&cmdline);
                let steam_game = std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|e| steam_env(&e));
                let mut paths = Vec::new();
                if let Some(w) = &wine {
                    if let Some(u) = wine_to_unix(w) {
                        paths.push(u);
                    }
                    paths.push(w.clone());
                }
                if let Some(e) = &exe {
                    paths.push(e.clone());
                }
                let name = wine.as_deref().or(exe.as_deref()).map(|p| file_name(p).to_string()).unwrap_or_else(|| comm.clone());
                let c = Cached { start, name, paths, steam_game };
                self.cache.insert(pid, c.clone());
                c
            }
        };
        Some(ProcessInfo { pid, ppid, name: c.name, paths: c.paths, steam_game: c.steam_game })
    }
}

impl GameProbe for LinuxProbe {
    fn foreground(&mut self) -> Result<Option<Foreground>, ()> {
        let own = std::process::id();
        let r = match &self.fg {
            ForegroundSource::X11(a) => a.query().map(|o| o.map(|(pid, fullscreen)| Foreground { pid, fullscreen })),
            ForegroundSource::Hyprland(p) => hyprland_active(p),
            ForegroundSource::Sway(p) => sway_active(p),
            ForegroundSource::None => Err(()),
        };
        r.map(|o| o.filter(|f| f.pid != own))
    }

    fn process(&mut self, pid: u32) -> Option<ProcessInfo> {
        self.read(pid)
    }

    fn all_processes(&mut self) -> Vec<ProcessInfo> {
        let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
        let pids: Vec<u32> = dir.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect();
        let out: Vec<ProcessInfo> = pids.iter().filter_map(|&p| self.read(p)).collect();
        self.cache.retain(|pid, _| pids.contains(pid));
        out
    }

    fn library_roots(&mut self) -> Vec<LibraryRoot> {
        discover_library_roots()
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn xdg(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(default))
}

/// `install_path` values from a Heroic/Legendary `installed.json` (object or array form).
pub fn parse_install_paths(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let items: Vec<&serde_json::Value> = match &v {
        serde_json::Value::Object(m) => match m.get("installed").and_then(|i| i.as_array()) {
            Some(a) => a.iter().collect(),
            None => m.values().collect(),
        },
        serde_json::Value::Array(a) => a.iter().collect(),
        _ => Vec::new(),
    };
    items.iter().filter_map(|i| i.get("install_path").and_then(|p| p.as_str()).filter(|p| !p.is_empty()).map(str::to_string)).collect()
}

/// Collect game library folders from launcher files of the current user.
pub fn discover_library_roots() -> Vec<LibraryRoot> {
    let mut roots = Vec::new();
    let data = xdg("XDG_DATA_HOME", ".local/share");
    let config = xdg("XDG_CONFIG_HOME", ".config");
    let h = home();

    // Steam (native, legacy symlink, Flatpak): libraryfolders.vdf lists every library.
    for steam in [data.join("Steam"), h.join(".steam/steam"), h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam")] {
        let vdf = steam.join("steamapps/libraryfolders.vdf");
        let Ok(t) = std::fs::read_to_string(&vdf) else { continue };
        let mut libs = parse_steam_libraryfolders(&t);
        libs.push(steam.to_string_lossy().into_owned());
        for l in libs {
            let real = std::fs::canonicalize(&l).map(|p| p.to_string_lossy().into_owned()).unwrap_or(l);
            roots.push(LibraryRoot::new(&format!("{real}/steamapps/common"), "Steam"));
        }
    }

    // Heroic (Epic via Legendary, GOG, Amazon via Nile), native and Flatpak.
    for heroic in [config.join("heroic"), h.join(".var/app/com.heroicgameslauncher.hgl/config/heroic")] {
        for (file, launcher) in [
            ("legendaryConfig/legendary/installed.json", "Epic Games (Heroic)"),
            ("gog_store/installed.json", "GOG (Heroic)"),
            ("nile_config/nile/installed.json", "Amazon Games (Heroic)"),
        ] {
            if let Ok(t) = std::fs::read_to_string(heroic.join(file)) {
                for p in parse_install_paths(&t) {
                    roots.push(LibraryRoot::new(&p, launcher));
                }
            }
        }
    }
    // Legendary used directly.
    if let Ok(t) = std::fs::read_to_string(config.join("legendary/installed.json")) {
        for p in parse_install_paths(&t) {
            roots.push(LibraryRoot::new(&p, "Epic Games (Legendary)"));
        }
    }

    // Lutris: one YAML per game; the game's `exe:` locates its folder.
    if let Ok(dir) = std::fs::read_dir(config.join("lutris/games")).or_else(|_| std::fs::read_dir(data.join("lutris/games"))) {
        for e in dir.flatten() {
            let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
            for line in t.lines() {
                let l = line.trim();
                if let Some(v) = l.strip_prefix("exe:") {
                    let v = v.trim().trim_matches(['\'', '"']);
                    if v.starts_with('/')
                        && let Some(parent) = Path::new(v).parent()
                    {
                        roots.push(LibraryRoot::new(&parent.to_string_lossy(), "Lutris"));
                    }
                }
            }
        }
    }

    // itch.io app.
    let itch = config.join("itch/apps");
    if itch.is_dir() {
        roots.push(LibraryRoot::new(&itch.to_string_lossy(), "itch.io"));
    }

    roots.sort_by(|a, b| a.path.cmp(&b.path));
    roots.dedup_by(|a, b| a.path == b.path);
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_spaces_and_parens() {
        let s = "1234 (Game (x) 1) S 77 1234 1234 0 -1 4194560 100 0 0 0 5 6 0 0 20 0 30 0 987654 1000 100 18446744073709551615";
        let (comm, ppid, start) = parse_stat(s).unwrap();
        assert_eq!(comm, "Game (x) 1");
        assert_eq!(ppid, 77);
        assert_eq!(start, 987654);
    }

    #[test]
    fn wine_and_steam_env() {
        assert_eq!(wine_argv0(b"Z:\\home\\u\\Games\\Foo\\Foo.exe\0-dx12\0").as_deref(), Some("Z:\\home\\u\\Games\\Foo\\Foo.exe"));
        assert_eq!(wine_argv0(b"/usr/bin/bash\0"), None);
        assert_eq!(wine_to_unix("Z:\\home\\u\\Foo.exe").as_deref(), Some("/home/u/Foo.exe"));
        assert!(steam_env(b"HOME=/h\0SteamGameId=570\0"));
        assert!(!steam_env(b"SteamAppId=0\0"));
        assert!(!steam_env(b"PATH=/bin\0"));
    }

    #[test]
    fn compositor_ipc_parsing() {
        let h = br#"{"address":"0x1","pid":4242,"fullscreen":2,"class":"steam_app_570"}"#;
        assert_eq!(parse_hyprland_active(h).unwrap(), Some(Foreground { pid: 4242, fullscreen: true }));
        assert_eq!(parse_hyprland_active(br#"{"pid":7,"fullscreen":false}"#).unwrap(), Some(Foreground { pid: 7, fullscreen: false }));
        assert_eq!(parse_hyprland_active(b"{}").unwrap(), None);
        let tree = br#"{"focused":false,"nodes":[{"focused":false,"nodes":[],"floating_nodes":[{"focused":true,"pid":99,"fullscreen_mode":1}]}]}"#;
        assert_eq!(parse_sway_tree(tree).unwrap(), Some(Foreground { pid: 99, fullscreen: true }));
    }

    #[test]
    fn heroic_installed_json() {
        let legendary = r#"{"Fortnite":{"install_path":"/home/u/Games/Heroic/Fortnite"},"X":{"install_path":""}}"#;
        assert_eq!(parse_install_paths(legendary), vec!["/home/u/Games/Heroic/Fortnite"]);
        let gog = r#"{"installed":[{"install_path":"/home/u/Games/Heroic/Witcher"}]}"#;
        assert_eq!(parse_install_paths(gog), vec!["/home/u/Games/Heroic/Witcher"]);
    }
}
