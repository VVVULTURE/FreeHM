//! Linux platform layer.
//!
//! Overlay backend, in order of preference:
//! 1. Wayland session with `wlr-layer-shell` (KDE Plasma, Hyprland, Sway, niri, COSMIC, …):
//!    a layer surface on the Overlay layer.
//! 2. X11 session, or a Wayland session without layer-shell (GNOME) through XWayland: an
//!    override-redirect ARGB window.
//! 3. Neither available: the overlay is reported unsupported (telemetry, tray and settings
//!    keep working).
//!
//! The UI thread is a plain loop over the event channel; X11 events, Wayland, the tray,
//! D-Bus and IPC each run on their own thread and post events to it.

pub mod autostart;
pub mod dbus;
pub mod ipc;
pub mod permissions;
pub mod probe;
pub mod sensors;
pub mod tray;
pub mod wayland;
pub mod x11;

use crate::app::{App, AppEvent, EventSink, Placement, Platform, TrayAction, TrayState};
use crate::sensors::nvml::NvmlProvider;
use freehm_core::config::Config;
use freehm_core::hotkey::Hotkey;
use freehm_core::ipc::{AutostartStatus, HelperStatus, MonitorInfo};
use freehm_core::render::Pixmap;
use freehm_core::telemetry::Provider;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Holds the single-instance lock (an `flock` on `$XDG_RUNTIME_DIR/freehm/app.lock`) for
/// the lifetime of the process; the kernel releases it if the process dies.
pub struct InstanceGuard(#[allow(dead_code)] std::fs::File);

pub fn single_instance() -> Option<InstanceGuard> {
    let dir = ipc::ensure_runtime_dir().map_err(|e| eprintln!("{e}")).ok()?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("app.lock")).ok()?;
    rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive).ok()?;
    Some(InstanceGuard(f))
}

/// Single instance of the settings window (Wayland does not let us raise the open one).
pub fn settings_instance() -> Option<InstanceGuard> {
    let dir = ipc::ensure_runtime_dir().ok()?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("settings.lock")).ok()?;
    rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive).ok()?;
    Some(InstanceGuard(f))
}

pub fn set_dpi_awareness() {}

pub fn ipc_connect() -> std::io::Result<Box<dyn crate::ipc::ReadWrite + Send>> {
    Ok(Box::new(ipc::connect()?))
}

enum Overlay {
    Wayland(wayland::Wayland),
    X11,
    None(String),
}

enum Shortcuts {
    X11,
    Portal(dbus::GlobalShortcuts),
    None(String),
}

struct LinuxPlatform {
    overlay: Overlay,
    /// The X connection: overlay window on X11/XWayland, key grabs on X11.
    x11: Option<x11::X11>,
    shortcuts: Shortcuts,
    tray: Option<tray::Tray>,
    tray_state: Option<TrayState>,
    tray_failed: bool,
    sink: EventSink,
    settings_child: Option<std::process::Child>,
    session: String,
    backend_note: String,
}

fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
}

impl Platform for LinuxPlatform {
    fn overlay_backend(&self) -> String {
        match &self.overlay {
            Overlay::Wayland(_) => "Wayland layer-shell (overlay layer, no keyboard focus, empty input region)".into(),
            Overlay::X11 => format!("X11 override-redirect ARGB window with empty input shape{}", self.backend_note),
            Overlay::None(_) => "none".into(),
        }
    }

    fn overlay_unsupported(&self) -> Option<String> {
        match &self.overlay {
            Overlay::None(why) => Some(why.clone()),
            _ => None,
        }
    }

    fn monitors(&mut self) -> Vec<MonitorInfo> {
        match &self.overlay {
            Overlay::Wayland(w) => w.monitors(),
            _ => self.x11.as_ref().map(|x| x.monitors()).unwrap_or_default(),
        }
    }

    fn overlay_show(&mut self, pm: &Pixmap, place: &Placement) {
        match &self.overlay {
            Overlay::Wayland(w) => w.show(pm, place),
            Overlay::X11 => {
                if let Some(x) = self.x11.as_mut() {
                    x.show(pm, place);
                }
            }
            Overlay::None(_) => {}
        }
    }

    fn overlay_hide(&mut self) {
        match &self.overlay {
            Overlay::Wayland(w) => w.hide(),
            Overlay::X11 => {
                if let Some(x) = self.x11.as_mut() {
                    x.hide();
                }
            }
            Overlay::None(_) => {}
        }
    }

    /// Linux display servers have no per-window "exclude from capture" flag; screen
    /// recorders capture the overlay like any other window.
    fn overlay_set_capture_excluded(&mut self, _on: bool) {}

    fn overlay_raise(&mut self) {
        if let (Overlay::X11, Some(x)) = (&self.overlay, self.x11.as_ref()) {
            x.raise();
        }
    }

    fn notify(&mut self, title: &str, text: &str) {
        dbus::notify(title, text);
    }

    fn register_hotkey(&mut self, hk: &Hotkey) -> Result<String, String> {
        match &self.shortcuts {
            Shortcuts::X11 => self.x11.as_ref().ok_or("no X connection")?.grab_hotkey(hk),
            Shortcuts::Portal(p) => {
                p.bind(hk);
                Err("waiting for the desktop to confirm the shortcut (it may show a dialog)".into())
            }
            Shortcuts::None(why) => Err(why.clone()),
        }
    }

    fn update_tray(&mut self, st: &TrayState) {
        if self.tray_state.as_ref() == Some(st) {
            return;
        }
        self.tray_state = Some(st.clone());
        match &self.tray {
            Some(t) => t.update(st),
            None if self.tray_failed => {}
            None => match tray::Tray::new(self.sink.clone(), st.clone()) {
                Ok(t) => self.tray = Some(t),
                Err(e) => {
                    log::warn!("tray icon unavailable: {e} (run `freehm` again to open settings)");
                    self.tray_failed = true;
                }
            },
        }
    }

    fn autostart_status(&self) -> AutostartStatus {
        autostart::status()
    }

    fn set_autostart(&mut self, on: bool) -> Result<(), String> {
        autostart::set(on)
    }

    fn open_settings(&mut self) -> Result<(), String> {
        if let Some(c) = self.settings_child.as_mut()
            && matches!(c.try_wait(), Ok(None))
        {
            // Already open. Wayland does not let one client raise another's window.
            return Ok(());
        }
        let exe = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from).map(Ok).unwrap_or_else(std::env::current_exe).map_err(|e| e.to_string())?;
        let child = std::process::Command::new(exe).arg("--settings").spawn().map_err(|e| e.to_string())?;
        self.settings_child = Some(child);
        Ok(())
    }

    fn helper_status(&mut self) -> HelperStatus {
        HelperStatus {
            applicable: false,
            message: "Linux needs no helper service; CPU power uses the one-time sensor permission setup".into(),
            ..Default::default()
        }
    }

    fn platform_name(&self) -> String {
        self.session.clone()
    }

    fn request_exit(&mut self) {}
}

pub fn run(cfg: Config, cfg_path: std::path::PathBuf, notes: Vec<String>, mock: bool, open_settings: bool) -> i32 {
    let (tx, rx) = mpsc::channel::<AppEvent>();
    // The UI loop blocks on the channel itself, so posting an event is enough to wake it.
    let sink = EventSink::new(tx, Arc::new(|| {}));

    if let Err(e) = ipc::start_server(sink.clone()) {
        log::error!("IPC endpoint unavailable ({e})");
        return 2;
    }

    let on_wayland = wayland_session();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session = format!("Linux ({}{})", if on_wayland { "Wayland" } else { "X11" }, if desktop.is_empty() { String::new() } else { format!(", {desktop}") });

    // Overlay backend. The X connection is only opened when it is needed: on GNOME,
    // connecting would start Xwayland on demand.
    let mut backend_note = String::new();
    let wl = if on_wayland {
        wayland::Wayland::connect(sink.clone())
            .map_err(|e| {
                log::info!("Wayland overlay unavailable: {e}; trying XWayland");
                backend_note = format!(" via XWayland ({e}); it may not appear above native Wayland fullscreen windows");
            })
            .ok()
    } else {
        None
    };
    let (overlay, x11) = match wl {
        Some(w) => (Overlay::Wayland(w), None),
        None => match x11::X11::connect() {
            Ok(x) => {
                if !x.composited() {
                    backend_note.push_str("; no compositing manager is running, so the background is opaque");
                }
                x.spawn_event_thread(sink.clone());
                (Overlay::X11, Some(x))
            }
            Err(e) => (Overlay::None(format!("no usable display server for the overlay: {e}")), None),
        },
    };

    // Global shortcut: X11 grabs only see keys sent to X clients, so on Wayland use the portal.
    let shortcuts = if !on_wayland && x11.is_some() {
        Shortcuts::X11
    } else {
        match dbus::GlobalShortcuts::connect(sink.clone()) {
            Ok(p) => Shortcuts::Portal(p),
            Err(e) => Shortcuts::None(format!("{e}. Bind a key to `freehm --toggle` in your desktop's keyboard-shortcut settings instead")),
        }
    };

    dbus::watch_sleep(sink.clone());

    let providers: Vec<Box<dyn Provider>> = if mock {
        vec![Box::new(freehm_core::telemetry::mock::MockProvider::demo().0)]
    } else {
        // Order = precedence when two providers supply the same metric.
        vec![
            Box::new(NvmlProvider::new()),
            Box::new(sensors::DrmGpuProvider::new()),
            Box::new(sensors::CpuProvider::new()),
            Box::new(sensors::MemoryProvider),
            Box::new(sensors::NoFramesProvider),
        ]
    };
    let snap_sink = sink.clone();
    let telemetry = freehm_core::telemetry::TelemetryThread::spawn(providers, Box::new(move |s| snap_sink.send(AppEvent::Snapshot(s))));
    let fg = probe::ForegroundSource::detect(x11.as_ref().map(|x| x.active_window_source()), on_wayland);
    log::info!("foreground window source: {}", fg.describe());
    let detector = crate::detect::spawn(Box::new(probe::LinuxProbe::new(fg)), cfg.detection.clone(), sink.clone());

    let mut plat = LinuxPlatform {
        overlay,
        x11,
        shortcuts,
        tray: None,
        tray_state: None,
        tray_failed: false,
        sink: sink.clone(),
        settings_child: None,
        session,
        backend_note,
    };
    let update = crate::updater::Updater::new(sink.clone(), !mock);
    let mut app = App::new(cfg, cfg_path, notes, telemetry, detector, update, &mut plat);
    log::info!("FreeHM {} running on {} — overlay: {}", freehm_core::APP_VERSION, plat.session, plat.overlay_backend());
    if open_settings {
        app.handle(AppEvent::Tray(TrayAction::OpenSettings), &mut plat);
    }

    const TICK: Duration = Duration::from_secs(1);
    let mut next_tick = Instant::now() + TICK;
    loop {
        let now = Instant::now();
        if now >= next_tick {
            app.tick(&mut plat);
            next_tick = now + TICK;
        }
        match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
            Ok(ev) => app.handle(ev, &mut plat),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if app.is_exiting() {
            break;
        }
    }

    // Orderly shutdown: tray icon removed, overlay surface destroyed, socket removed.
    drop(app);
    plat.tray.take();
    plat.overlay_hide();
    drop(plat);
    ipc::remove_socket();
    0
}
