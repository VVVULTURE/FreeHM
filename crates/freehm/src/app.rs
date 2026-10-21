//! Application controller. Runs on the platform's UI thread; background threads (telemetry,
//! game detection, IPC, tray/hotkey on Linux) communicate with it only through
//! [`AppEvent`]s, so no state is shared across threads.

use crate::detect::{DetectorCmd, GameUpdate};
use freehm_core::config::{Anchor, Config, MonitorChoice};
use freehm_core::hotkey::Hotkey;
use freehm_core::ipc::{AutostartStatus, GameStatus, HelperStatus, HotkeyStatus, MonitorInfo, OverlayStatus, Request, Response, Status};
use freehm_core::metrics::Snapshot;
use freehm_core::policy::{self, Inputs, VisibilityState};
use freehm_core::render::{Pixmap, Renderer, Row, Style, build_rows};
use freehm_core::telemetry::{Command, PollRequest, TelemetryThread};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub enum AppEvent {
    Snapshot(Snapshot),
    Game(GameUpdate),
    Ipc(Request, mpsc::Sender<Response>),
    Hotkey,
    /// Outcome of an asynchronous shortcut registration (Wayland portal).
    #[cfg_attr(windows, allow(dead_code))]
    HotkeyResult(Result<String, String>),
    Tray(TrayAction),
    DisplayChanged,
    Resumed,
    /// Foreground window changed (Windows): re-assert topmost and re-check the game quickly.
    ForegroundChanged,
    /// The session is ending (Windows WM_ENDSESSION).
    #[cfg_attr(not(windows), allow(dead_code))]
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSettings,
    ToggleOverlay,
    ToggleDetection,
    ToggleAutostart,
    Quit,
}

/// Thread-safe handle for posting events to the UI thread. `wake` interrupts the platform
/// event loop (PostMessage on Windows, a pipe write on Linux).
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::Sender<AppEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl EventSink {
    pub fn new(tx: mpsc::Sender<AppEvent>, wake: Arc<dyn Fn() + Send + Sync>) -> EventSink {
        EventSink { tx, wake }
    }
    pub fn send(&self, e: AppEvent) {
        if self.tx.send(e).is_ok() {
            (self.wake)();
        }
    }
}

/// Where to put the overlay. `x`/`y` are physical pixels in desktop coordinates; backends
/// that position relative to an output (Wayland layer-shell) use the anchor and margins.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub monitor: MonitorInfo,
    pub x: i32,
    pub y: i32,
    pub anchor: Anchor,
    /// Logical-pixel distance from the anchored edges (or custom offset).
    pub margin_x: i32,
    pub margin_y: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrayState {
    pub overlay_visible: bool,
    pub detection_enabled: bool,
    pub autostart: bool,
    pub tooltip: String,
}

/// Services the platform layer provides to the controller (all called on the UI thread).
pub trait Platform {
    fn overlay_backend(&self) -> String;
    /// `Some(reason)` when this session cannot display an overlay at all.
    fn overlay_unsupported(&self) -> Option<String>;
    fn monitors(&mut self) -> Vec<MonitorInfo>;
    fn overlay_show(&mut self, pm: &Pixmap, place: &Placement);
    fn overlay_hide(&mut self);
    fn overlay_set_capture_excluded(&mut self, on: bool);
    /// Re-assert z-order above other topmost windows (after a foreground change).
    fn overlay_raise(&mut self) {}
    /// Register (replacing any previous) global shortcut. Ok = mechanism description.
    fn register_hotkey(&mut self, hk: &Hotkey) -> Result<String, String>;
    fn update_tray(&mut self, st: &TrayState);
    fn autostart_status(&self) -> AutostartStatus;
    fn set_autostart(&mut self, on: bool) -> Result<(), String>;
    fn open_settings(&mut self) -> Result<(), String>;
    fn helper_status(&mut self) -> HelperStatus;
    fn platform_name(&self) -> String;
    /// The configuration was loaded or changed (for platform-held settings).
    fn on_config(&mut self, _cfg: &Config) {}
    fn request_exit(&mut self);
}

pub struct App {
    cfg: Config,
    cfg_path: PathBuf,
    notes: Vec<String>,
    vis: VisibilityState,
    visible: bool,
    vis_reason: String,
    snapshot: Snapshot,
    game: GameUpdate,
    telemetry: TelemetryThread,
    last_plan: Option<(Duration, PollRequest)>,
    detector: mpsc::Sender<DetectorCmd>,
    renderer: Renderer,
    renderer_font: (freehm_core::config::FontChoice, freehm_core::config::FontWeight),
    font_note: Option<String>,
    last_drawn: Option<(Vec<Row>, Style, Placement)>,
    settings_live_until: Option<Instant>,
    hotkey: HotkeyStatus,
    monitors: Vec<MonitorInfo>,
    monitors_at: Instant,
    capture_excluded: bool,
    exiting: bool,
}

impl App {
    pub fn new(
        cfg: Config,
        cfg_path: PathBuf,
        notes: Vec<String>,
        telemetry: TelemetryThread,
        detector: mpsc::Sender<DetectorCmd>,
        p: &mut dyn Platform,
    ) -> App {
        let (renderer, font_note) = Renderer::new(&cfg.overlay.font, cfg.overlay.font_weight);
        let renderer_font = (cfg.overlay.font.clone(), cfg.overlay.font_weight);
        let mut app = App {
            cfg,
            cfg_path,
            notes,
            vis: VisibilityState::default(),
            visible: false,
            vis_reason: String::new(),
            snapshot: Snapshot::default(),
            game: GameUpdate::default(),
            telemetry,
            last_plan: None,
            detector,
            renderer,
            renderer_font,
            font_note,
            last_drawn: None,
            settings_live_until: None,
            hotkey: HotkeyStatus::default(),
            monitors: Vec::new(),
            monitors_at: Instant::now(),
            capture_excluded: false,
            exiting: false,
        };
        p.on_config(&app.cfg);
        app.monitors = p.monitors();
        app.apply_hotkey(p);
        app.sync_autostart(p);
        app.refresh(p);
        app
    }

    pub fn is_exiting(&self) -> bool {
        self.exiting
    }

    pub fn handle(&mut self, ev: AppEvent, p: &mut dyn Platform) {
        match ev {
            AppEvent::Snapshot(s) => {
                self.snapshot = s;
                self.redraw(p, false);
            }
            AppEvent::Game(g) => {
                if g.active.as_ref().map(|a| a.pid) != self.game.active.as_ref().map(|a| a.pid) {
                    match &g.active {
                        Some(a) => log::info!("game active: {} (pid {}) — {}", a.name, a.pid, a.reason),
                        None => log::info!("no game active"),
                    }
                }
                self.game = g;
                self.refresh(p);
            }
            AppEvent::Hotkey => self.toggle(p),
            AppEvent::HotkeyResult(r) => {
                match &r {
                    Ok(m) => log::info!("shortcut {} registered via {m}", self.hotkey.shortcut),
                    Err(e) => log::warn!("shortcut {} could not be registered: {e}", self.hotkey.shortcut),
                }
                let (registered, error, mechanism) = match r {
                    Ok(m) => (true, None, m),
                    Err(e) => (false, Some(e), String::new()),
                };
                self.hotkey = HotkeyStatus { shortcut: self.hotkey.shortcut.clone(), registered, error, mechanism };
            }
            AppEvent::Tray(a) => match a {
                TrayAction::OpenSettings => self.open_settings(p),
                TrayAction::ToggleOverlay => self.toggle(p),
                TrayAction::ToggleDetection => {
                    let mut c = self.cfg.clone();
                    c.detection.auto_detect = !c.detection.auto_detect;
                    let _ = self.apply_config(c, p);
                }
                TrayAction::ToggleAutostart => {
                    let mut c = self.cfg.clone();
                    c.general.start_with_system = !c.general.start_with_system;
                    let _ = self.apply_config(c, p);
                }
                TrayAction::Quit => self.quit(p),
            },
            AppEvent::Ipc(req, reply) => {
                let resp = self.handle_request(req, p);
                let _ = reply.send(resp);
            }
            AppEvent::DisplayChanged => {
                self.monitors = p.monitors();
                self.monitors_at = Instant::now();
                self.redraw(p, true);
            }
            AppEvent::Resumed => {
                log::info!("system resumed");
                self.telemetry.send(Command::Resume);
                let _ = self.detector.send(DetectorCmd::Poke);
                self.monitors = p.monitors();
                self.redraw(p, true);
            }
            AppEvent::ForegroundChanged => {
                let _ = self.detector.send(DetectorCmd::Poke);
                if self.visible {
                    p.overlay_raise();
                }
            }
            AppEvent::Quit => self.quit(p),
        }
    }

    /// Called about once a second by the platform loop.
    pub fn tick(&mut self, p: &mut dyn Platform) {
        if let Some(t) = self.settings_live_until
            && Instant::now() >= t
        {
            self.settings_live_until = None;
            self.refresh(p);
        }
        // Monitor layouts can change without a notification reaching us (e.g. a monitor
        // turned off while the session was locked); a cheap periodic re-enumeration fixes it.
        if self.monitors_at.elapsed() > Duration::from_secs(5) {
            let m = p.monitors();
            self.monitors_at = Instant::now();
            if m != self.monitors {
                self.monitors = m;
                self.redraw(p, true);
            }
        }
    }

    fn toggle(&mut self, p: &mut dyn Platform) {
        self.vis.toggle(self.visible, self.game.active.as_ref().map(|g| g.pid));
        self.refresh(p);
    }

    fn quit(&mut self, p: &mut dyn Platform) {
        if self.exiting {
            return;
        }
        log::info!("quitting");
        self.exiting = true;
        p.overlay_hide();
        let _ = self.detector.send(DetectorCmd::Quit);
        p.request_exit();
    }

    fn open_settings(&mut self, p: &mut dyn Platform) {
        if let Err(e) = p.open_settings() {
            log::warn!("could not open settings: {e}");
        }
    }

    fn handle_request(&mut self, req: Request, p: &mut dyn Platform) -> Response {
        match req {
            Request::Hello { .. } => Response::Hello { version: freehm_core::ipc::PROTOCOL_VERSION, app_version: freehm_core::APP_VERSION.into() },
            Request::GetStatus { live } => {
                if live {
                    let was = self.settings_live_until.is_some();
                    self.settings_live_until = Some(Instant::now() + Duration::from_secs(3));
                    if !was {
                        self.refresh(p);
                    }
                }
                Response::Status(Box::new(self.status(p)))
            }
            Request::ApplyConfig(c) => match self.apply_config(*c, p) {
                Ok(()) => Response::Status(Box::new(self.status(p))),
                Err(errs) => Response::Rejected(errs),
            },
            Request::ToggleOverlay => {
                self.toggle(p);
                Response::Ok
            }
            Request::SetOverlayVisible(v) => {
                self.vis.set(v, self.game.active.as_ref().map(|g| g.pid));
                self.refresh(p);
                Response::Ok
            }
            Request::OpenSettings => {
                self.open_settings(p);
                Response::Ok
            }
            Request::RefreshPermissions => {
                self.telemetry.send(Command::Resume);
                Response::Ok
            }
            Request::Quit => {
                self.quit(p);
                Response::Ok
            }
        }
    }

    /// Validate, apply live, and persist. No restart or elevation is ever needed.
    pub fn apply_config(&mut self, mut new: Config, p: &mut dyn Platform) -> Result<(), Vec<String>> {
        new.validate()?;
        new.sanitize();
        let old = std::mem::replace(&mut self.cfg, new);
        p.on_config(&self.cfg);
        if old.hotkeys != self.cfg.hotkeys {
            self.apply_hotkey(p);
        }
        if old.general.start_with_system != self.cfg.general.start_with_system {
            self.sync_autostart(p);
        }
        if old.detection != self.cfg.detection {
            let _ = self.detector.send(DetectorCmd::Config(self.cfg.detection.clone()));
        }
        if (old.overlay.font.clone(), old.overlay.font_weight) != (self.cfg.overlay.font.clone(), self.cfg.overlay.font_weight) {
            let (r, note) = Renderer::new(&self.cfg.overlay.font, self.cfg.overlay.font_weight);
            self.renderer = r;
            self.font_note = note;
            self.renderer_font = (self.cfg.overlay.font.clone(), self.cfg.overlay.font_weight);
        }
        if let Err(e) = self.cfg.save(&self.cfg_path) {
            log::error!("could not save configuration to {}: {e}", self.cfg_path.display());
            self.notes.push(format!("could not save configuration: {e}"));
        }
        self.refresh(p);
        self.redraw(p, true);
        Ok(())
    }

    fn apply_hotkey(&mut self, p: &mut dyn Platform) {
        let text = self.cfg.hotkeys.toggle_overlay.clone();
        self.hotkey = match Hotkey::parse(&text) {
            Ok(hk) => match p.register_hotkey(&hk) {
                Ok(mech) => {
                    log::info!("shortcut {hk} registered via {mech}");
                    HotkeyStatus { shortcut: hk.to_string(), registered: true, error: None, mechanism: mech }
                }
                Err(e) => {
                    log::warn!("shortcut {hk} could not be registered: {e}");
                    HotkeyStatus { shortcut: hk.to_string(), registered: false, error: Some(e), mechanism: String::new() }
                }
            },
            Err(e) => HotkeyStatus { shortcut: text, registered: false, error: Some(e), mechanism: String::new() },
        };
    }

    fn sync_autostart(&mut self, p: &mut dyn Platform) {
        let want = self.cfg.general.start_with_system;
        if p.autostart_status().enabled != want
            && let Err(e) = p.set_autostart(want)
        {
            log::warn!("could not {} automatic start: {e}", if want { "enable" } else { "disable" });
        }
    }

    /// Recompute visibility and the telemetry plan; called after any state change.
    fn refresh(&mut self, p: &mut dyn Platform) {
        let unsupported = p.overlay_unsupported();
        let inputs = Inputs {
            supported: unsupported.is_none(),
            active_game: self.game.active.as_ref().map(|g| g.pid),
            show_on_desktop: self.cfg.overlay.show_on_desktop,
        };
        let (visible, reason) = self.vis.decide(inputs);
        let changed = visible != self.visible;
        self.visible = visible;
        self.vis_reason = reason;

        let pids: Vec<u32> = if !self.game.active_tree.is_empty() {
            self.game.active_tree.clone()
        } else {
            self.game.foreground.as_ref().map(|f| vec![f.pid]).unwrap_or_default()
        };
        let plan = policy::poll_plan(&self.cfg, self.visible, self.settings_live_until.is_some(), &pids);
        if self.last_plan.as_ref() != Some(&plan) {
            self.telemetry.send(Command::Configure { interval: plan.0, req: plan.1.clone() });
            self.last_plan = Some(plan);
        }
        if self.capture_excluded != self.cfg.overlay.hide_from_capture {
            self.capture_excluded = self.cfg.overlay.hide_from_capture;
            p.overlay_set_capture_excluded(self.capture_excluded);
        }
        p.update_tray(&TrayState {
            overlay_visible: self.visible,
            detection_enabled: self.cfg.detection.auto_detect,
            autostart: self.cfg.general.start_with_system,
            tooltip: match &self.game.active {
                Some(g) => format!("FreeHM — {}", g.name),
                None => "FreeHM — no game detected".into(),
            },
        });
        if changed {
            if self.visible {
                self.redraw(p, true);
            } else {
                p.overlay_hide();
                self.last_drawn = None;
            }
        }
    }

    fn target_monitor(&self) -> Option<MonitorInfo> {
        let by_name = match &self.cfg.overlay.monitor {
            MonitorChoice::Named(n) => self.monitors.iter().find(|m| &m.name == n),
            MonitorChoice::Primary => None,
        };
        by_name.or_else(|| self.monitors.iter().find(|m| m.primary)).or_else(|| self.monitors.first()).cloned()
    }

    /// Render and present if anything visible changed. `force` re-presents even if the text
    /// is identical (monitor/DPI changes, re-show).
    fn redraw(&mut self, p: &mut dyn Platform, force: bool) {
        if !self.visible {
            return;
        }
        let Some(mon) = self.target_monitor() else {
            return;
        };
        let rows = build_rows(&self.snapshot, &self.cfg);
        let style = Style::from_config(&self.cfg, mon.scale);
        if !force
            && let Some((r, s, _)) = &self.last_drawn
            && *r == rows
            && *s == style
        {
            return; // nothing changed: no rasterization, no window update
        }
        let pm = self.renderer.render(&rows, &style);
        if pm.width == 0 {
            p.overlay_hide();
            self.last_drawn = None;
            return;
        }
        let place = compute_placement(&mon, &self.cfg, pm.width, pm.height);
        p.overlay_show(&pm, &place);
        self.last_drawn = Some((rows, style, place));
    }

    fn status(&mut self, p: &mut dyn Platform) -> Status {
        let mut notes = self.notes.clone();
        if let Some(n) = &self.font_note {
            notes.push(n.clone());
        }
        Status {
            app_version: freehm_core::APP_VERSION.into(),
            platform: p.platform_name(),
            config: self.cfg.clone(),
            config_path: self.cfg_path.display().to_string(),
            log_path: crate::paths::log_file().display().to_string(),
            snapshot: self.snapshot.clone(),
            overlay: OverlayStatus {
                visible: self.visible,
                reason: self.vis_reason.clone(),
                backend: p.overlay_backend(),
                unsupported: p.overlay_unsupported(),
                warning: self.game.warning.clone(),
            },
            hotkey: self.hotkey.clone(),
            game: GameStatus { detection_enabled: self.cfg.detection.auto_detect, active: self.game.active.clone(), foreground: self.game.foreground.clone() },
            helper: p.helper_status(),
            autostart: p.autostart_status(),
            monitors: self.monitors.clone(),
            notes,
        }
    }
}

/// Position the overlay on a monitor according to anchor, margins and DPI scale.
pub fn compute_placement(mon: &MonitorInfo, cfg: &Config, w: u32, h: u32) -> Placement {
    let o = &cfg.overlay;
    let s = mon.scale;
    let (mx, my) = ((o.margin_x * s).round() as i32, (o.margin_y * s).round() as i32);
    let (w, h) = (w as i32, h as i32);
    let (mw, mh) = (mon.width as i32, mon.height as i32);
    let (rx, ry) = match o.anchor {
        Anchor::TopLeft => (mx, my),
        Anchor::TopCenter => ((mw - w) / 2, my),
        Anchor::TopRight => (mw - w - mx, my),
        Anchor::BottomLeft => (mx, mh - h - my),
        Anchor::BottomCenter => ((mw - w) / 2, mh - h - my),
        Anchor::BottomRight => (mw - w - mx, mh - h - my),
        Anchor::Custom => ((o.offset_x * s).round() as i32, (o.offset_y * s).round() as i32),
    };
    // Keep the overlay fully on its monitor.
    let rx = rx.clamp(0, (mw - w).max(0));
    let ry = ry.clamp(0, (mh - h).max(0));
    let (margin_x, margin_y) = match o.anchor {
        Anchor::Custom => (o.offset_x.round() as i32, o.offset_y.round() as i32),
        _ => (o.margin_x.round() as i32, o.margin_y.round() as i32),
    };
    Placement { monitor: mon.clone(), x: mon.x + rx, y: mon.y + ry, anchor: o.anchor, margin_x, margin_y }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, y: i32, w: u32, h: u32, scale: f32) -> MonitorInfo {
        MonitorInfo { name: "m".into(), primary: true, x, y, width: w, height: h, scale }
    }

    #[test]
    fn placement_corners_and_scaling() {
        let mut c = Config::default();
        c.overlay.margin_x = 10.0;
        c.overlay.margin_y = 20.0;
        let m = mon(0, 0, 2560, 1440, 1.0);
        c.overlay.anchor = Anchor::TopLeft;
        let p = compute_placement(&m, &c, 100, 50);
        assert_eq!((p.x, p.y), (10, 20));
        c.overlay.anchor = Anchor::BottomRight;
        let p = compute_placement(&m, &c, 100, 50);
        assert_eq!((p.x, p.y), (2560 - 100 - 10, 1440 - 50 - 20));
        // 150 % scaling doubles nothing but margins scale by 1.5.
        let m2 = mon(0, 0, 3840, 2160, 1.5);
        c.overlay.anchor = Anchor::TopRight;
        let p = compute_placement(&m2, &c, 100, 50);
        assert_eq!((p.x, p.y), (3840 - 100 - 15, 30));
    }

    #[test]
    fn placement_on_secondary_monitor_and_clamped() {
        let mut c = Config::default();
        // Secondary monitor left of the primary (negative desktop coordinates).
        let m = mon(-1920, 0, 1920, 1080, 1.0);
        c.overlay.anchor = Anchor::TopLeft;
        let p = compute_placement(&m, &c, 100, 50);
        assert_eq!(p.x, -1920 + 16);
        c.overlay.anchor = Anchor::Custom;
        c.overlay.offset_x = 5000.0;
        c.overlay.offset_y = -40.0;
        let p = compute_placement(&m, &c, 100, 50);
        assert_eq!((p.x, p.y), (-1920 + 1920 - 100, 0), "custom position is kept on the monitor");
        c.overlay.anchor = Anchor::BottomCenter;
        let p = compute_placement(&m, &c, 100, 50);
        assert_eq!(p.x, -1920 + (1920 - 100) / 2);
    }
}
