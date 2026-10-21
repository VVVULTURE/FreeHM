//! Game detection rules.
//!
//! Platform code supplies cheap, non-invasive process facts (executable path, command line,
//! parent pid, a "SteamGameId" environment flag on Linux, whether the foreground window
//! covers a monitor). This module decides whether a process is a game, and why.
//!
//! Order of rules (first match wins):
//! 1. user exclusion list → not a game
//! 2. user manual game list → game
//! 3. automatic detection disabled → not a game
//! 4. built-in list of launchers, helpers, browsers, crash handlers, installers → not a game
//! 5. executable inside a launcher's game library (Steam, Epic, GOG, Battle.net, EA, Ubisoft,
//!    Xbox, itch.io, Heroic, Lutris — discovered from local launcher files) → game
//! 6. Linux: process started by Steam for a game (`SteamGameId` env) → game
//! 7. optional: unknown application whose foreground window covers a whole monitor → game
//!
//! A launcher being open is never enough: launcher executables live outside their game
//! library folders and are also on the built-in list.

use crate::config::{DetectionConfig, ShowFor};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    /// File name, e.g. `game.exe` or `game.x86_64`.
    pub name: String,
    /// Paths that identify the program: the executable path and, for Wine/Proton
    /// processes, the Windows path from argv[0].
    pub paths: Vec<String>,
    /// Linux: the process environment contains SteamGameId/SteamAppId (set by Steam for games).
    pub steam_game: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryRoot {
    /// Normalised (lower-case, forward slashes, trailing slash).
    pub path: String,
    pub launcher: String,
}

impl LibraryRoot {
    pub fn new(path: &str, launcher: &str) -> LibraryRoot {
        let mut p = normalize(path);
        if !p.ends_with('/') {
            p.push('/');
        }
        LibraryRoot { path: p, launcher: launcher.to_string() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Verdict {
    Game(String),
    NotGame(String),
}

impl Verdict {
    pub fn is_game(&self) -> bool {
        matches!(self, Verdict::Game(_))
    }
    pub fn reason(&self) -> &str {
        match self {
            Verdict::Game(r) | Verdict::NotGame(r) => r,
        }
    }
}

/// Lower-case, forward slashes, no `\\?\` prefix — so paths from different APIs compare.
pub fn normalize(p: &str) -> String {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    p.replace('\\', "/").to_lowercase()
}

pub fn file_name(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

/// Executables that are never games: launchers and their helpers, anti-cheat services,
/// crash reporters, installers, browsers, media players, system processes and FreeHM itself.
const NOT_GAMES: &[&str] = &[
    // FreeHM
    "freehm",
    "freehm-helper",
    // Steam
    "steam",
    "steamwebhelper",
    "steamservice",
    "steamerrorreporter",
    "steamerrorreporter64",
    "gameoverlayui",
    "gameoverlayui64",
    "steam-runtime-launcher-service",
    "pressure-vessel-wrap",
    "pressure-vessel-adverb",
    "reaper",
    "srt-bwrap",
    "steam-runtime-supervisor",
    // Epic
    "epicgameslauncher",
    "epicwebhelper",
    "epiconlineservices",
    "epiconlineserviceshost",
    "epiconlineservicesuserhelper",
    "eosoverlayrenderer-win64-shipping",
    "eosoverlayrenderer-win32-shipping",
    "unrealcefsubprocess",
    // GOG
    "galaxyclient",
    "galaxyclient helper",
    "galaxycommunication",
    "gogalaxynotifications",
    "galaxyupdater",
    // Battle.net
    "battle.net",
    "agent",
    "blizzard error",
    "blizzardbrowser",
    "battle.net helper",
    // EA
    "eadesktop",
    "eabackgroundservice",
    "eaconnect_microsoft",
    "eacefsubprocess",
    "origin",
    "originwebhelperservice",
    "link2ea",
    // Ubisoft
    "upc",
    "ubisoftconnect",
    "ubisoftgamelauncher",
    "ubisoftgamelauncher64",
    "uplaywebcore",
    "upc_crash_reporter",
    // itch / Xbox / others
    "itch",
    "itch-setup",
    "butler",
    "xboxpcapp",
    "xboxapp",
    "gamingservices",
    "gamingservicesnet",
    "gamebar",
    "gamebarftserver",
    "heroic",
    "legendary",
    "gogdl",
    "nile",
    "lutris",
    "bottles",
    "playnite.desktopapp",
    "playnite.fullscreenapp",
    "gamescope",
    "amazon games",
    "amazon games ui",
    "riotclientservices",
    "riotclientux",
    "riotclientuxrender",
    "riot client",
    // anti-cheat / crash handling / installers
    "easyanticheat",
    "easyanticheat_eos",
    "easyanticheat_setup",
    "easyanticheat_eos_setup",
    "beservice",
    "beservice_x64",
    "battleye launcher",
    "vgc",
    "vgtray",
    "unitycrashhandler64",
    "unitycrashhandler32",
    "crashreportclient",
    "crashpad_handler",
    "crashreporter",
    "werfault",
    "vc_redist.x64",
    "vc_redist.x86",
    "dxsetup",
    "dxwebsetup",
    "ue4prereqsetup_x64",
    "ueprereqsetup_x64",
    "unins000",
    "uninstall",
    "setup",
    "installer",
    "dotnetfx",
    // Wine / Proton infrastructure
    "wineserver",
    "winedevice",
    "services",
    "plugplay",
    "rpcss",
    "explorer",
    "conhost",
    "start",
    "winedbg",
    "tabtip",
    "wine64-preloader",
    "wine-preloader",
    "wine64",
    "wine",
    // Browsers / media / communication / streaming
    "chrome",
    "msedge",
    "msedgewebview2",
    "firefox",
    "opera",
    "opera_gx",
    "brave",
    "vivaldi",
    "librewolf",
    "chromium",
    "vlc",
    "mpv",
    "mpc-hc64",
    "mpc-be64",
    "potplayermini64",
    "wmplayer",
    "spotify",
    "discord",
    "discordcanary",
    "teams",
    "ms-teams",
    "zoom",
    "slack",
    "obs64",
    "obs",
    "streamlabs obs",
    "nvidia app",
    "nvidia overlay",
    "nvcontainer",
    // Windows shell & system
    "dwm",
    "applicationframehost",
    "searchhost",
    "startmenuexperiencehost",
    "shellexperiencehost",
    "textinputhost",
    "lockapp",
    "systemsettings",
    "taskmgr",
    "mmc",
    "cmd",
    "powershell",
    "pwsh",
    "windowsterminal",
    "code",
    "devenv",
];

/// Folder-name patterns that identify launcher game libraries even when the launcher's own
/// files could not be read (e.g. a library on a second drive with no launcher installed).
const LIBRARY_PATTERNS: &[(&str, &str)] = &[
    ("/steamapps/common/", "Steam"),
    ("/gog games/", "GOG"),
    ("/gog galaxy/games/", "GOG"),
    ("/ubisoft game launcher/games/", "Ubisoft Connect"),
    ("/ea games/", "EA app"),
    ("/xboxgames/", "Xbox / Game Pass"),
    ("/modifiablewindowsapps/", "Xbox / Game Pass"),
    ("/itch/apps/", "itch.io"),
    ("/games/heroic/", "Heroic"),
    ("/epic games/", "Epic Games"),
];

fn stem(name: &str) -> String {
    let n = name.to_lowercase();
    for ext in [".exe", ".x86_64", ".x86", ".sh", ".appimage", ".bin"] {
        if let Some(s) = n.strip_suffix(ext) {
            return s.to_string();
        }
    }
    n
}

/// Does a user list entry (exe name or full path, case-insensitive) match the process?
pub fn entry_matches(entry: &str, p: &ProcessInfo) -> bool {
    let e = entry.trim();
    if e.is_empty() {
        return false;
    }
    if e.contains(['/', '\\']) {
        let ne = normalize(e);
        p.paths.iter().any(|x| normalize(x) == ne)
    } else {
        let es = stem(e);
        stem(&p.name) == es || p.paths.iter().any(|x| stem(file_name(x)) == es)
    }
}

pub struct Classifier {
    pub roots: Vec<LibraryRoot>,
}

impl Classifier {
    pub fn new(roots: Vec<LibraryRoot>) -> Classifier {
        Classifier { roots }
    }

    /// `fullscreen`: the process owns the foreground window and it covers a whole monitor.
    pub fn classify(&self, p: &ProcessInfo, cfg: &DetectionConfig, fullscreen: bool) -> Verdict {
        if let Some(e) = cfg.excluded.iter().find(|e| entry_matches(e, p)) {
            return Verdict::NotGame(format!("excluded by user ({e})"));
        }
        if let Some(e) = cfg.manual_games.iter().find(|e| entry_matches(e, p)) {
            return Verdict::Game(format!("in your game list ({e})"));
        }
        if cfg.show_for == ShowFor::SelectedGamesOnly {
            return Verdict::NotGame("overlay is limited to your game list".into());
        }
        if !cfg.auto_detect {
            return Verdict::NotGame("automatic detection is off".into());
        }
        if NOT_GAMES.contains(&stem(&p.name).as_str()) {
            return Verdict::NotGame(format!("{} is a launcher/helper/system application", p.name));
        }
        // Library membership is checked on every identifying path, before the system-path
        // test: a Proton game's executable is the Wine preloader in /usr/lib, while its
        // argv[0] is the game's Windows path inside the Steam library.
        for path in &p.paths {
            let np = normalize(path);
            if let Some(r) = self.roots.iter().find(|r| np.starts_with(&r.path)) {
                return Verdict::Game(format!("installed in {} library", r.launcher));
            }
            if let Some((_, launcher)) = LIBRARY_PATTERNS.iter().find(|(pat, _)| np.contains(pat)) {
                // The Epic launcher and its services live under "Epic Games/" too.
                if *launcher == "Epic Games" && (np.contains("/epic games/launcher/") || np.contains("/epic games/epic online services/")) {
                    continue;
                }
                return Verdict::Game(format!("installed in a {launcher} library folder"));
            }
        }
        if p.steam_game {
            return Verdict::Game("launched by Steam as a game".into());
        }
        let system = p
            .paths
            .iter()
            .map(|x| normalize(x))
            .any(|np| ["c:/windows/", "/usr/bin/", "/usr/lib/", "/usr/libexec/", "/bin/", "/usr/share/"].iter().any(|s| np.starts_with(s)));
        if system {
            return Verdict::NotGame("system program".into());
        }
        if fullscreen && cfg.detect_fullscreen_apps {
            return Verdict::Game("unknown application running fullscreen".into());
        }
        Verdict::NotGame("not in a known game library (add it to your game list if this is a game)".into())
    }
}

/// Find the game for a foreground process: the process itself, or (for wrappers such as
/// gamescope, a game's own launcher, or a Proton/Wine chain) its descendants.
/// Returns the chosen pid and verdict.
pub fn resolve_game<'a>(
    root: &'a ProcessInfo,
    all: &'a [ProcessInfo],
    classify: &dyn Fn(&ProcessInfo, bool) -> Verdict,
    root_fullscreen: bool,
) -> (&'a ProcessInfo, Verdict) {
    let v = classify(root, root_fullscreen);
    if v.is_game() {
        return (root, v);
    }
    // Breadth-first over descendants, bounded so a pathological tree can't stall us.
    let mut frontier = vec![root.pid];
    let mut visited = 0;
    while !frontier.is_empty() && visited < 512 {
        let mut next = Vec::new();
        for parent in frontier {
            for c in all.iter().filter(|c| c.ppid == parent && c.pid != parent) {
                visited += 1;
                let cv = classify(c, false);
                if cv.is_game() {
                    return (c, cv);
                }
                next.push(c.pid);
            }
        }
        frontier = next;
    }
    (root, v)
}

/// All descendants of `pid` (inclusive), used to find which process in a game's tree is
/// actually presenting frames.
pub fn process_tree(pid: u32, all: &[ProcessInfo]) -> Vec<u32> {
    let mut out = vec![pid];
    let mut i = 0;
    while i < out.len() && out.len() < 512 {
        let parent = out[i];
        let children: Vec<u32> = all.iter().filter(|c| c.ppid == parent && c.pid != parent && !out.contains(&c.pid)).map(|c| c.pid).collect();
        out.extend(children);
        i += 1;
    }
    out
}

/// Extract library paths from Steam's `libraryfolders.vdf` (KeyValues text format).
pub fn parse_steam_libraryfolders(vdf: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in vdf.lines() {
        let toks = quoted_tokens(line);
        if toks.len() == 2 && toks[0].eq_ignore_ascii_case("path") {
            out.push(toks[1].replace("\\\\", "\\"));
        }
    }
    out
}

fn quoted_tokens(line: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut inq = false;
    let mut esc = false;
    for c in line.chars() {
        if inq {
            if esc {
                cur.push('\\');
                cur.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                toks.push(std::mem::take(&mut cur));
                inq = false;
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            inq = true;
        }
    }
    toks
}

/// `InstallLocation` from an Epic Games Launcher `.item` manifest (JSON).
pub fn parse_epic_manifest(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let loc = v.get("InstallLocation")?.as_str()?;
    (!loc.is_empty()).then(|| loc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32, path: &str) -> ProcessInfo {
        ProcessInfo { pid, ppid, name: file_name(path).to_string(), paths: vec![path.to_string()], steam_game: false }
    }

    fn classifier() -> Classifier {
        Classifier::new(vec![
            LibraryRoot::new(r"D:\SteamLibrary\steamapps\common", "Steam"),
            LibraryRoot::new(r"C:\Program Files\Epic Games\Fortnite", "Epic Games"),
        ])
    }

    #[test]
    fn steam_game_detected_and_steam_itself_not() {
        let c = classifier();
        let cfg = DetectionConfig::default();
        assert!(c.classify(&proc(1, 0, r"D:\SteamLibrary\steamapps\common\Hades\Hades.exe"), &cfg, false).is_game());
        let steam = proc(2, 0, r"C:\Program Files (x86)\Steam\steam.exe");
        assert!(!c.classify(&steam, &cfg, false).is_game());
        let helper = proc(3, 2, r"C:\Program Files (x86)\Steam\bin\cef\cef.win7x64\steamwebhelper.exe");
        assert!(!c.classify(&helper, &cfg, true).is_game(), "launcher stays non-game even fullscreen");
    }

    #[test]
    fn library_pattern_without_launcher_files() {
        let c = Classifier::new(vec![]);
        let cfg = DetectionConfig::default();
        assert!(c.classify(&proc(1, 0, r"E:\XboxGames\Forza\Content\forza.exe"), &cfg, false).is_game());
        assert!(c.classify(&proc(1, 0, r"F:\GOG Games\Witcher 3\bin\x64\witcher3.exe"), &cfg, false).is_game());
        assert!(c.classify(&proc(1, 0, "/home/u/.local/share/Steam/steamapps/common/Game/game.x86_64"), &cfg, false).is_game());
        // Epic launcher binary lives under "Epic Games/Launcher" and must not match.
        let launcher = proc(1, 0, r"C:\Program Files (x86)\Epic Games\Launcher\Portal\Binaries\Win64\SomeTool.exe");
        assert!(!c.classify(&launcher, &cfg, false).is_game());
    }

    #[test]
    fn crash_handlers_in_game_folder_are_not_games() {
        let c = classifier();
        let p = proc(1, 0, r"D:\SteamLibrary\steamapps\common\Game\UnityCrashHandler64.exe");
        assert!(!c.classify(&p, &DetectionConfig::default(), false).is_game());
    }

    #[test]
    fn manual_and_excluded_lists() {
        let c = classifier();
        let mut cfg = DetectionConfig::default();
        let standalone = proc(1, 0, r"C:\Games\MyIndie\indie.exe");
        assert!(!c.classify(&standalone, &cfg, false).is_game());
        cfg.manual_games.push("INDIE.EXE".into());
        assert!(c.classify(&standalone, &cfg, false).is_game());
        // Exclusion beats both the manual list and the library rule.
        cfg.excluded.push(r"c:\games\myindie\indie.exe".into());
        assert!(!c.classify(&standalone, &cfg, false).is_game());
        let lib = proc(2, 0, r"D:\SteamLibrary\steamapps\common\Tool\tool.exe");
        cfg.excluded.push("tool".into());
        assert!(!c.classify(&lib, &cfg, false).is_game());
    }

    #[test]
    fn auto_detection_disabled() {
        let c = classifier();
        let mut cfg = DetectionConfig { auto_detect: false, ..Default::default() };
        let g = proc(1, 0, r"D:\SteamLibrary\steamapps\common\Hades\Hades.exe");
        assert!(!c.classify(&g, &cfg, false).is_game());
        cfg.manual_games.push("hades.exe".into());
        assert!(c.classify(&g, &cfg, false).is_game(), "manual list still works");
    }

    #[test]
    fn selected_games_only() {
        let c = classifier();
        let mut cfg = DetectionConfig { show_for: ShowFor::SelectedGamesOnly, ..Default::default() };
        let g = proc(1, 0, r"D:\SteamLibrary\steamapps\common\Hades\Hades.exe");
        assert!(!c.classify(&g, &cfg, true).is_game());
        cfg.manual_games.push("Hades".into());
        assert!(c.classify(&g, &cfg, false).is_game());
    }

    #[test]
    fn unknown_launcher_game_detected_by_fullscreen_heuristic() {
        let c = classifier();
        let mut cfg = DetectionConfig::default();
        let g = proc(1, 0, r"C:\Odd Launcher\titles\shooter\shooter.exe");
        assert!(!c.classify(&g, &cfg, false).is_game());
        assert!(c.classify(&g, &cfg, true).is_game());
        cfg.detect_fullscreen_apps = false;
        assert!(!c.classify(&g, &cfg, true).is_game());
        // Browsers in fullscreen video are not games.
        assert!(!c.classify(&proc(2, 0, r"C:\Program Files\Google\Chrome\Application\chrome.exe"), &DetectionConfig::default(), true).is_game());
        // System programs never.
        assert!(!c.classify(&proc(3, 0, r"C:\Windows\System32\mspaint.exe"), &DetectionConfig::default(), true).is_game());
    }

    #[test]
    fn proton_game_via_argv0_and_steam_env() {
        let c = Classifier::new(vec![]);
        let cfg = DetectionConfig::default();
        // Proton: exe is the Wine preloader, argv[0] is the Windows path in the library.
        let p = ProcessInfo {
            pid: 10,
            ppid: 9,
            name: "Game.exe".into(),
            paths: vec![r"Z:\home\u\.local\share\Steam\steamapps\common\SomeGame\Game.exe".into(), "/usr/lib/wine/wine64-preloader".into()],
            steam_game: true,
        };
        assert!(c.classify(&p, &cfg, false).is_game());
        // Outside a library, the SteamGameId environment flag identifies it.
        let p2 = ProcessInfo { paths: vec![r"C:\games\x\Game.exe".into(), "/usr/lib/wine/wine64-preloader".into()], ..p.clone() };
        assert!(c.classify(&p2, &cfg, false).is_game());
        // Without either, a Wine process under /usr/lib is not treated as a game.
        let p3 = ProcessInfo { steam_game: false, ..p2.clone() };
        assert!(!c.classify(&p3, &cfg, true).is_game());
        // Steam's own processes inherit SteamGameId-like variables in some setups.
        let steamproc = ProcessInfo { name: "steam".into(), paths: vec!["/home/u/.steam/ubuntu12_32/steam".into()], ..p };
        assert!(!c.classify(&steamproc, &cfg, false).is_game());
        // Wine infrastructure processes started for a game are not the game.
        let ws = ProcessInfo { name: "wineserver".into(), paths: vec!["/usr/bin/wineserver".into()], steam_game: true, ..p2 };
        assert!(!c.classify(&ws, &cfg, false).is_game());
    }

    #[test]
    fn nested_launcher_resolves_to_game_child() {
        let c = classifier();
        let cfg = DetectionConfig::default();
        let all = vec![
            proc(100, 1, r"C:\Odd Launcher\launcher.exe"),
            proc(101, 100, r"C:\Odd Launcher\updater.exe"),
            proc(102, 101, r"D:\SteamLibrary\steamapps\common\Game\Game.exe"),
            proc(200, 1, r"C:\Other\thing.exe"),
        ];
        let f = |p: &ProcessInfo, fs: bool| c.classify(p, &cfg, fs);
        let (g, v) = resolve_game(&all[0], &all, &f, false);
        assert_eq!(g.pid, 102);
        assert!(v.is_game());
        let (g, v) = resolve_game(&all[3], &all, &f, false);
        assert_eq!(g.pid, 200);
        assert!(!v.is_game());
    }

    #[test]
    fn process_tree_collects_descendants_and_survives_cycles() {
        let all = vec![proc(1, 0, "a"), proc(2, 1, "b"), proc(3, 2, "c"), proc(4, 9, "d"), proc(5, 5, "self")];
        let mut t = process_tree(1, &all);
        t.sort();
        assert_eq!(t, vec![1, 2, 3]);
        assert_eq!(process_tree(5, &all), vec![5]);
    }

    #[test]
    fn steam_vdf_parsing() {
        let vdf = r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"label"		""
		"apps" { "228980" "1" }
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}"#;
        assert_eq!(parse_steam_libraryfolders(vdf), vec![r"C:\Program Files (x86)\Steam", r"D:\SteamLibrary"]);
    }

    #[test]
    fn epic_manifest_parsing() {
        assert_eq!(parse_epic_manifest(r#"{"DisplayName":"X","InstallLocation":"C:\\Games\\X"}"#).as_deref(), Some(r"C:\Games\X"));
        assert_eq!(parse_epic_manifest("not json"), None);
        assert_eq!(parse_epic_manifest(r#"{"InstallLocation":""}"#), None);
    }
}
