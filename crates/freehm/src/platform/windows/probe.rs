//! Windows game-detection facts. Processes are opened only with
//! `PROCESS_QUERY_LIMITED_INFORMATION` (image path and exit status) — the least access
//! right Windows has, granted even for most protected processes, and unrelated to memory
//! access. Game library locations come from the launchers' own local files and registry
//! entries; nothing is fetched from the network.

use super::util::{OwnedHandle, class_name, from_wide, reg_string, reg_subkeys};
use crate::detect::{Foreground, GameProbe};
use freehm_core::gamedetect::{LibraryRoot, ProcessInfo, file_name, parse_epic_manifest, parse_steam_libraryfolders};
use std::collections::HashMap;
use windows::Win32::Foundation::{HWND, RECT, STILL_ACTIVE};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId};
use windows::core::PWSTR;

#[derive(Default)]
pub struct WinProbe {
    /// pid → image path, refreshed from each process snapshot (paths of dead pids dropped).
    paths: HashMap<u32, Option<String>>,
}

fn image_path(pid: u32) -> Option<String> {
    if pid == 0 || pid == 4 {
        return None; // System Idle Process / System
    }
    unsafe {
        let h = OwnedHandle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut code = 0u32;
        if GetExitCodeProcess(h.0, &mut code).is_ok() && code != STILL_ACTIVE.0 as u32 {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn is_alive(pid: u32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let h = OwnedHandle(h);
                let mut code = 0u32;
                GetExitCodeProcess(h.0, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32
            }
            Err(_) => false,
        }
    }
}

fn covers_monitor(hwnd: HWND) -> bool {
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

impl GameProbe for WinProbe {
    fn foreground(&mut self) -> Result<Option<Foreground>, ()> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return Ok(None);
            }
            // The desktop itself "covers the monitor" but is never a game.
            let cls = class_name(hwnd);
            if cls == "Progman" || cls == "WorkerW" || cls == "Shell_TrayWnd" {
                return Ok(None);
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return Ok(None);
            }
            Ok(Some(Foreground { pid, fullscreen: covers_monitor(hwnd) }))
        }
    }

    fn process(&mut self, pid: u32) -> Option<ProcessInfo> {
        if !is_alive(pid) {
            self.paths.remove(&pid);
            return None;
        }
        let path = match self.paths.get(&pid) {
            Some(p) => p.clone(),
            None => {
                let p = image_path(pid);
                self.paths.insert(pid, p.clone());
                p
            }
        };
        let path = path?;
        Some(ProcessInfo { pid, ppid: 0, name: file_name(&path).to_string(), paths: vec![path], steam_game: false })
    }

    fn all_processes(&mut self) -> Vec<ProcessInfo> {
        let mut out = Vec::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
            let snap = OwnedHandle(snap);
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap.0, &mut e).is_ok();
            let mut alive = Vec::new();
            while ok {
                let pid = e.th32ProcessID;
                alive.push(pid);
                let name = from_wide(&e.szExeFile);
                let path = self.paths.entry(pid).or_insert_with(|| image_path(pid)).clone();
                out.push(ProcessInfo { pid, ppid: e.th32ParentProcessID, name, paths: path.into_iter().collect(), steam_game: false });
                ok = Process32NextW(snap.0, &mut e).is_ok();
            }
            self.paths.retain(|pid, _| alive.contains(pid));
        }
        out
    }

    fn library_roots(&mut self) -> Vec<LibraryRoot> {
        discover_library_roots()
    }
}

/// The monitor (device name) of the foreground window while a Direct3D program holds
/// exclusive fullscreen. Windows draws nothing else on that monitor then: click-through
/// layered windows such as the overlay are left out, and any other window drawn over the
/// game takes it out of exclusive mode. Other monitors are unaffected.
pub fn exclusive_fullscreen_monitor() -> Option<String> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow};
    unsafe {
        if SHQueryUserNotificationState().ok()? != QUNS_RUNNING_D3D_FULL_SCREEN {
            return None;
        }
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        GetMonitorInfoW(mon, &mut mi.monitorInfo).as_bool().then(|| from_wide(&mi.szDevice))
    }
}

/// Collect game library folders from launcher files on this machine.
pub fn discover_library_roots() -> Vec<LibraryRoot> {
    let mut roots = Vec::new();

    // Steam: libraryfolders.vdf lists every library.
    if let Some(steam) = reg_string(HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath") {
        let vdf = std::path::Path::new(&steam).join("steamapps").join("libraryfolders.vdf");
        let mut libs = std::fs::read_to_string(&vdf).map(|t| parse_steam_libraryfolders(&t)).unwrap_or_default();
        libs.push(steam.clone());
        for l in libs {
            roots.push(LibraryRoot::new(&format!("{l}/steamapps/common"), "Steam"));
        }
    }

    // Epic Games: one JSON manifest per installed game.
    let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    if let Ok(dir) = std::fs::read_dir(std::path::Path::new(&program_data).join(r"Epic\EpicGamesLauncher\Data\Manifests")) {
        for e in dir.flatten() {
            if e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("item"))
                && let Ok(t) = std::fs::read_to_string(e.path())
                && let Some(loc) = parse_epic_manifest(&t)
            {
                roots.push(LibraryRoot::new(&loc, "Epic Games"));
            }
        }
    }

    // GOG, Ubisoft Connect and EA write per-game install keys.
    for (base, value, launcher) in [
        (r"SOFTWARE\WOW6432Node\GOG.com\Games", "path", "GOG"),
        (r"SOFTWARE\GOG.com\Games", "path", "GOG"),
        (r"SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs", "InstallDir", "Ubisoft Connect"),
        (r"SOFTWARE\WOW6432Node\EA Games", "Install Dir", "EA app"),
        (r"SOFTWARE\WOW6432Node\Electronic Arts", "Install Dir", "EA app"),
    ] {
        for k in reg_subkeys(HKEY_LOCAL_MACHINE, base) {
            if let Some(p) = reg_string(HKEY_LOCAL_MACHINE, &format!(r"{base}\{k}"), value) {
                roots.push(LibraryRoot::new(&p, launcher));
            }
        }
    }

    // Battle.net (and other launchers) register uninstall entries with an install location.
    for base in [r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"] {
        for k in reg_subkeys(HKEY_LOCAL_MACHINE, base) {
            let key = format!(r"{base}\{k}");
            let publisher = reg_string(HKEY_LOCAL_MACHINE, &key, "Publisher").unwrap_or_default();
            let uninstall = reg_string(HKEY_LOCAL_MACHINE, &key, "UninstallString").unwrap_or_default().to_lowercase();
            if publisher.contains("Blizzard")
                && uninstall.contains("battle.net")
                && let Some(loc) = reg_string(HKEY_LOCAL_MACHINE, &key, "InstallLocation").filter(|l| !l.to_lowercase().contains("battle.net"))
            {
                roots.push(LibraryRoot::new(&loc, "Battle.net"));
            }
        }
    }

    // Xbox app / PC Game Pass default library on each drive.
    for d in b'C'..=b'Z' {
        let p = format!("{}:\\XboxGames", d as char);
        if std::path::Path::new(&p).is_dir() {
            roots.push(LibraryRoot::new(&p, "Xbox / Game Pass"));
        }
    }

    // itch.io app.
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = std::path::Path::new(&appdata).join("itch").join("apps");
        if p.is_dir() {
            roots.push(LibraryRoot::new(&p.to_string_lossy(), "itch.io"));
        }
    }

    roots.sort_by(|a, b| a.path.cmp(&b.path));
    roots.dedup_by(|a, b| a.path == b.path);
    roots
}
