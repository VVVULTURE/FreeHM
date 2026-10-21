//! Game detection thread. Runs every `detection.interval_ms` (default 2 s) or immediately
//! when poked (foreground change). Each tick normally costs one foreground-window query and
//! one cached comparison; the full process list is only read when the foreground process
//! changes, or every tick on sessions without foreground information (most Wayland desktops).

use crate::app::{AppEvent, EventSink};
use freehm_core::config::DetectionConfig;
use freehm_core::gamedetect::{Classifier, LibraryRoot, ProcessInfo, Verdict, process_tree, resolve_game};
use freehm_core::ipc::GameInfo;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GameUpdate {
    pub active: Option<GameInfo>,
    /// The active game's process and its descendants (for frame statistics).
    pub active_tree: Vec<u32>,
    pub foreground: Option<GameInfo>,
    /// e.g. "exclusive fullscreen detected: the overlay may be hidden by the game".
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Foreground {
    pub pid: u32,
    /// The foreground window covers a whole monitor.
    pub fullscreen: bool,
}

/// Platform facts needed for detection. All methods must be non-invasive: they may read
/// process metadata (path, parent, command line, environment flags) but never open a
/// process for memory access.
pub trait GameProbe: Send {
    /// `Ok(None)`: no window has focus. `Err(())`: this session cannot tell (Wayland).
    fn foreground(&mut self) -> Result<Option<Foreground>, ()>;
    fn process(&mut self, pid: u32) -> Option<ProcessInfo>;
    fn all_processes(&mut self) -> Vec<ProcessInfo>;
    fn library_roots(&mut self) -> Vec<LibraryRoot>;
    /// Warning about the current display mode, if any.
    fn display_warning(&mut self) -> Option<String> {
        None
    }
}

pub enum DetectorCmd {
    Config(DetectionConfig),
    Poke,
    Quit,
}

const ROOTS_REFRESH: Duration = Duration::from_secs(300);

pub fn spawn(mut probe: Box<dyn GameProbe>, cfg: DetectionConfig, sink: EventSink) -> mpsc::Sender<DetectorCmd> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("game-detect".into())
        .spawn(move || {
            let mut d = Detector { cfg, classifier: Classifier::new(probe.library_roots()), roots_at: Instant::now(), last: None, last_sent: None };
            loop {
                let wait = Duration::from_millis(d.cfg.interval_ms as u64);
                match rx.recv_timeout(wait) {
                    Ok(DetectorCmd::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                    Ok(DetectorCmd::Config(c)) => {
                        d.cfg = c;
                        d.last = None; // re-classify with the new rules
                    }
                    Ok(DetectorCmd::Poke) | Err(RecvTimeoutError::Timeout) => {}
                }
                if d.roots_at.elapsed() > ROOTS_REFRESH {
                    d.classifier = Classifier::new(probe.library_roots());
                    d.roots_at = Instant::now();
                }
                let upd = d.tick(&mut *probe);
                if d.last_sent.as_ref() != Some(&upd) {
                    d.last_sent = Some(upd.clone());
                    sink.send(AppEvent::Game(upd));
                }
            }
        })
        .expect("spawn game-detect thread");
    tx
}

struct Detector {
    cfg: DetectionConfig,
    classifier: Classifier,
    roots_at: Instant,
    /// Cache: (foreground pid, fullscreen flag) → result, valid while that process lives.
    last: Option<(Foreground, GameUpdate)>,
    last_sent: Option<GameUpdate>,
}

fn info(p: &ProcessInfo, v: &Verdict) -> GameInfo {
    GameInfo { pid: p.pid, name: p.name.clone(), path: p.paths.first().cloned().unwrap_or_default(), is_game: v.is_game(), reason: v.reason().to_string() }
}

impl Detector {
    fn tick(&mut self, probe: &mut dyn GameProbe) -> GameUpdate {
        let warning = probe.display_warning();
        match probe.foreground() {
            Ok(Some(fg)) => {
                // Fast path: same foreground process, still alive → reuse the decision.
                if let Some((last_fg, upd)) = &self.last
                    && *last_fg == fg
                    && probe.process(fg.pid).is_some()
                    && upd.active.as_ref().is_none_or(|a| a.pid == fg.pid || probe.process(a.pid).is_some())
                {
                    let mut u = upd.clone();
                    u.warning = warning;
                    return u;
                }
                let Some(fp) = probe.process(fg.pid) else {
                    self.last = None;
                    return GameUpdate { warning, ..Default::default() };
                };
                let all = probe.all_processes();
                let cfg = &self.cfg;
                let classifier = &self.classifier;
                let classify = |p: &ProcessInfo, fs: bool| classifier.classify(p, cfg, fs);
                let fv = classify(&fp, fg.fullscreen);
                let (gp, gv) = resolve_game(&fp, &all, &classify, fg.fullscreen);
                let upd = GameUpdate {
                    active: gv.is_game().then(|| info(gp, &gv)),
                    active_tree: if gv.is_game() { process_tree(gp.pid, &all) } else { Vec::new() },
                    foreground: Some(info(&fp, &fv)),
                    warning: if gv.is_game() { warning } else { None },
                };
                self.last = Some((fg, upd.clone()));
                upd
            }
            Ok(None) => {
                self.last = None;
                GameUpdate::default()
            }
            Err(()) => {
                // No foreground information: any running game counts. Prefer the game that
                // was already active so two running games don't make the overlay flip.
                let all = probe.all_processes();
                let prev = self.last_sent.as_ref().and_then(|u| u.active.as_ref()).map(|a| a.pid);
                let mut games: Vec<(&ProcessInfo, Verdict)> =
                    all.iter().map(|p| (p, self.classifier.classify(p, &self.cfg, false))).filter(|(_, v)| v.is_game()).collect();
                games.sort_by_key(|(p, _)| (Some(p.pid) != prev, std::cmp::Reverse(p.pid)));
                match games.first() {
                    Some((p, v)) => GameUpdate { active: Some(info(p, v)), active_tree: process_tree(p.pid, &all), foreground: None, warning },
                    None => GameUpdate::default(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeProbe {
        fg: Option<Result<Option<Foreground>, ()>>,
        procs: HashMap<u32, ProcessInfo>,
        all_calls: u32,
    }

    impl GameProbe for FakeProbe {
        fn foreground(&mut self) -> Result<Option<Foreground>, ()> {
            self.fg.unwrap_or(Ok(None))
        }
        fn process(&mut self, pid: u32) -> Option<ProcessInfo> {
            self.procs.get(&pid).cloned()
        }
        fn all_processes(&mut self) -> Vec<ProcessInfo> {
            self.all_calls += 1;
            self.procs.values().cloned().collect()
        }
        fn library_roots(&mut self) -> Vec<LibraryRoot> {
            vec![LibraryRoot::new(r"D:\Steam\steamapps\common", "Steam")]
        }
    }

    fn p(pid: u32, ppid: u32, path: &str) -> ProcessInfo {
        ProcessInfo { pid, ppid, name: freehm_core::gamedetect::file_name(path).into(), paths: vec![path.into()], steam_game: false }
    }

    fn detector(probe: &mut FakeProbe) -> Detector {
        Detector { cfg: DetectionConfig::default(), classifier: Classifier::new(probe.library_roots()), roots_at: Instant::now(), last: None, last_sent: None }
    }

    #[test]
    fn game_start_and_exit() {
        let mut probe = FakeProbe::default();
        probe.procs.insert(10, p(10, 1, r"D:\Steam\steamapps\common\G\g.exe"));
        probe.procs.insert(11, p(11, 10, r"D:\Steam\steamapps\common\G\g-render.exe"));
        probe.fg = Some(Ok(Some(Foreground { pid: 10, fullscreen: true })));
        let mut d = detector(&mut probe);
        let u = d.tick(&mut probe);
        assert_eq!(u.active.as_ref().unwrap().pid, 10);
        let mut tree = u.active_tree.clone();
        tree.sort();
        assert_eq!(tree, vec![10, 11], "child render process included for FPS");
        // Cached on the next tick: no process enumeration.
        let calls = probe.all_calls;
        d.tick(&mut probe);
        assert_eq!(probe.all_calls, calls);
        // Game exits; the desktop takes focus.
        probe.procs.remove(&10);
        probe.procs.remove(&11);
        probe.procs.insert(2, p(2, 1, r"C:\Windows\explorer.exe"));
        probe.fg = Some(Ok(Some(Foreground { pid: 2, fullscreen: false })));
        assert!(d.tick(&mut probe).active.is_none());
    }

    #[test]
    fn launcher_alone_is_not_a_game() {
        let mut probe = FakeProbe::default();
        probe.procs.insert(5, p(5, 1, r"C:\Program Files (x86)\Steam\steam.exe"));
        probe.fg = Some(Ok(Some(Foreground { pid: 5, fullscreen: false })));
        let mut d = detector(&mut probe);
        let u = d.tick(&mut probe);
        assert!(u.active.is_none());
        assert!(!u.foreground.unwrap().is_game);
    }

    #[test]
    fn config_change_reclassifies() {
        let mut probe = FakeProbe::default();
        probe.procs.insert(7, p(7, 1, r"C:\Tools\emu\emu.exe"));
        probe.fg = Some(Ok(Some(Foreground { pid: 7, fullscreen: false })));
        let mut d = detector(&mut probe);
        assert!(d.tick(&mut probe).active.is_none());
        d.cfg.manual_games.push("emu.exe".into());
        d.last = None;
        assert!(d.tick(&mut probe).active.is_some());
    }

    #[test]
    fn no_foreground_info_scans_processes_and_keeps_previous_game() {
        let mut probe = FakeProbe { fg: Some(Err(())), ..Default::default() };
        probe.procs.insert(20, p(20, 1, r"D:\Steam\steamapps\common\A\a.exe"));
        let mut d = detector(&mut probe);
        let u = d.tick(&mut probe);
        assert_eq!(u.active.as_ref().unwrap().pid, 20);
        d.last_sent = Some(u);
        probe.procs.insert(30, p(30, 1, r"D:\Steam\steamapps\common\B\b.exe"));
        assert_eq!(d.tick(&mut probe).active.unwrap().pid, 20, "stick with the running game");
    }
}
