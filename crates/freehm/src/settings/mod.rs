//! Settings window (`freehm --settings`).
//!
//! It runs as its own short-lived process so the background app never holds a GPU context
//! or UI memory. It talks to the running app over the per-user IPC endpoint: every edit is
//! sent (debounced) as a complete configuration, which the app validates, applies live and
//! saves. When the app is not running, edits are validated and written to the
//! configuration file instead, and take effect at the app's next start.

use eframe::egui;
mod actions;
mod pages;
mod preview;

use crate::fonts::SystemFonts;
use crate::ipc::Client;
use freehm_core::config::{Config, PermissionState};
use freehm_core::ipc::{PROTOCOL_VERSION, Request, Response, Status, UpdateMethod, UpdatePhase};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Overlay,
    Metrics,
    Hardware,
    Detection,
    Shortcuts,
    Startup,
    About,
}

impl Page {
    const ALL: [Page; 8] = [Page::General, Page::Overlay, Page::Metrics, Page::Hardware, Page::Detection, Page::Shortcuts, Page::Startup, Page::About];

    fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Overlay => "Overlay",
            Page::Metrics => "Metrics",
            Page::Hardware => "Hardware",
            Page::Detection => "Game detection",
            Page::Shortcuts => "Keyboard shortcut",
            Page::Startup => "Startup & permissions",
            Page::About => "About & diagnostics",
        }
    }
}

enum Cmd {
    Send(u64, Request),
}

/// State shared between the IPC worker thread and the UI.
#[derive(Default)]
struct Shared {
    connected: bool,
    connect_error: Option<String>,
    status: Option<Arc<Status>>,
    /// Incremented whenever `status` is replaced.
    status_gen: u64,
    /// Highest command sequence number fully processed before `status` was fetched, so the
    /// UI can tell a status that predates its latest edit from one that includes it.
    status_seq: u64,
    /// Problems reported for the last configuration that was sent (empty = accepted).
    rejected: Vec<String>,
    /// Last transport/application error (not configuration validation).
    error: Option<String>,
    /// Set when the configuration was saved to the file directly (app not running).
    saved_offline: bool,
    /// Ask the app to show the overlay while the Overlay page is open, so position and
    /// appearance changes can be seen on screen.
    show_overlay: bool,
}

fn connect() -> std::io::Result<Client> {
    let mut c = Client::connect()?;
    match c.call(&Request::Hello { version: PROTOCOL_VERSION })? {
        Response::Hello { version, .. } if version == PROTOCOL_VERSION => Ok(c),
        Response::Hello { version, app_version } => Err(std::io::Error::other(format!(
            "the running FreeHM ({app_version}) speaks protocol {version}, this window speaks {PROTOCOL_VERSION}; restart FreeHM"
        ))),
        other => Err(std::io::Error::other(format!("unexpected reply {other:?}"))),
    }
}

fn store_status(shared: &Mutex<Shared>, st: Box<Status>, seq: u64) {
    let mut s = shared.lock().unwrap();
    s.status = Some(Arc::from(st));
    s.status_gen += 1;
    s.status_seq = seq;
    s.connected = true;
    s.connect_error = None;
}

/// IPC worker: owns the connection, polls status twice a second (which also keeps the app
/// sampling every metric at full rate while this window is open) and forwards commands.
fn worker(shared: Arc<Mutex<Shared>>, rx: mpsc::Receiver<Cmd>, mut client: Option<Client>, wake: impl Fn()) {
    let mut done_seq = 0u64;
    let mut next_poll = Instant::now();
    let mut next_connect = Instant::now() + Duration::from_secs(1);
    loop {
        if client.is_none() && Instant::now() >= next_connect {
            match connect() {
                Ok(c) => {
                    client = Some(c);
                    next_poll = Instant::now();
                }
                Err(e) => {
                    let mut s = shared.lock().unwrap();
                    s.connected = false;
                    s.connect_error = Some(e.to_string());
                }
            }
            next_connect = Instant::now() + Duration::from_secs(1);
        }
        let wait = next_poll.saturating_duration_since(Instant::now()).min(Duration::from_millis(500));
        match rx.recv_timeout(wait) {
            Ok(Cmd::Send(seq, req)) => {
                let apply = matches!(req, Request::ApplyConfig(_));
                match client.as_mut() {
                    Some(c) => match c.call(&req) {
                        Ok(Response::Status(st)) => {
                            store_status(&shared, st, seq);
                            shared.lock().unwrap().rejected.clear();
                        }
                        Ok(Response::Rejected(errs)) => shared.lock().unwrap().rejected = errs,
                        Ok(Response::Error(e)) => shared.lock().unwrap().error = Some(e),
                        Ok(_) => {}
                        Err(e) => {
                            client = None;
                            let mut s = shared.lock().unwrap();
                            s.connected = false;
                            s.error = Some(format!("lost connection to FreeHM: {e}"));
                        }
                    },
                    None => {
                        if let Request::ApplyConfig(cfg) = req {
                            let mut s = shared.lock().unwrap();
                            match cfg.validate() {
                                Ok(()) => {
                                    let mut cfg = *cfg;
                                    cfg.sanitize();
                                    match cfg.save(&crate::paths::config_file()) {
                                        Ok(()) => {
                                            s.rejected.clear();
                                            s.saved_offline = true;
                                        }
                                        Err(e) => s.error = Some(format!("could not save the configuration: {e}")),
                                    }
                                }
                                Err(errs) => s.rejected = errs,
                            }
                        } else {
                            shared.lock().unwrap().error = Some("FreeHM is not running".into());
                        }
                    }
                }
                done_seq = seq;
                if apply || client.is_some() {
                    next_poll = Instant::now();
                }
                wake();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() >= next_poll {
            next_poll = Instant::now() + Duration::from_millis(500);
            if let Some(c) = client.as_mut() {
                let show_overlay = shared.lock().unwrap().show_overlay;
                match c.call(&Request::GetStatus { live: true, show_overlay }) {
                    Ok(Response::Status(st)) => store_status(&shared, st, done_seq),
                    Ok(_) => {}
                    Err(e) => {
                        client = None;
                        let mut s = shared.lock().unwrap();
                        s.connected = false;
                        s.connect_error = Some(e.to_string());
                    }
                }
                wake();
            }
        }
    }
}

/// Read the configuration file without the repair side effects of [`Config::load`] (which
/// renames a malformed file); the background app owns the file.
fn read_config_file() -> Config {
    std::fs::read_to_string(crate::paths::config_file()).ok().and_then(|t| Config::from_json(&t).ok()).map(|(c, _)| c).unwrap_or_default()
}

pub struct SettingsApp {
    page: Page,
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<Cmd>,
    seq: u64,
    /// Sequence number of the last configuration sent.
    sent_seq: u64,
    seen_gen: u64,
    /// The configuration being edited.
    pub draft: Config,
    /// `draft` as last sent (raw), and as the app will store it (sanitized).
    last_sent: Config,
    expected: Config,
    edited_at: Option<Instant>,
    /// Snapshot of the shared state for this frame.
    pub status: Option<Arc<Status>>,
    pub connected: bool,
    pub ui: pages::UiState,
    pub preview: preview::Preview,
    pub fonts: Arc<Mutex<Option<SystemFonts>>>,
    pub actions: actions::Actions,
    /// Navigation requested by a page (applied after the frame).
    pub page_request: Option<Page>,
}

impl SettingsApp {
    fn new(ctx: &egui::Context) -> SettingsApp {
        let shared = Arc::new(Mutex::new(Shared::default()));
        // Connect synchronously first so the window opens with the live configuration
        // instead of flashing file contents.
        let client = match connect() {
            Ok(mut c) => match c.call(&Request::GetStatus { live: true, show_overlay: false }) {
                Ok(Response::Status(st)) => {
                    store_status(&shared, st, 0);
                    Some(c)
                }
                _ => None,
            },
            Err(e) => {
                shared.lock().unwrap().connect_error = Some(e.to_string());
                None
            }
        };
        let draft = match &shared.lock().unwrap().status {
            Some(st) => st.config.clone(),
            None => read_config_file(),
        };
        let (tx, rx) = mpsc::channel();
        {
            let shared = shared.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("settings-ipc".into())
                .spawn(move || worker(shared, rx, client, move || ctx.request_repaint()))
                .expect("spawn IPC thread");
        }
        let fonts = Arc::new(Mutex::new(None));
        {
            let fonts = fonts.clone();
            let ctx = ctx.clone();
            let _ = std::thread::Builder::new().name("font-scan".into()).spawn(move || {
                let f = SystemFonts::scan();
                *fonts.lock().unwrap() = Some(f);
                ctx.request_repaint();
            });
        }
        let seen_gen = shared.lock().unwrap().status_gen;
        SettingsApp {
            page: Page::General,
            shared,
            tx,
            seq: 0,
            sent_seq: 0,
            seen_gen,
            last_sent: draft.clone(),
            expected: draft.clone(),
            draft,
            edited_at: None,
            status: None,
            connected: false,
            ui: pages::UiState::default(),
            preview: preview::Preview::new(),
            fonts,
            actions: actions::Actions::default(),
            page_request: None,
        }
    }

    /// Send a request that is not a configuration change.
    pub fn send(&mut self, req: Request) {
        self.seq += 1;
        let _ = self.tx.send(Cmd::Send(self.seq, req));
    }

    pub fn rejected(&self) -> Vec<String> {
        self.shared.lock().unwrap().rejected.clone()
    }

    fn sync_from_shared(&mut self) {
        let (status, gen_, seq, connected) = {
            let s = self.shared.lock().unwrap();
            (s.status.clone(), s.status_gen, s.status_seq, s.connected)
        };
        self.connected = connected;
        if gen_ != self.seen_gen {
            self.seen_gen = gen_;
            if let Some(st) = &status
                && seq >= self.sent_seq
                && self.draft == self.last_sent
                && st.config != self.expected
            {
                // Changed elsewhere (tray menu, another settings window, a rejected edit):
                // adopt the app's configuration.
                self.draft = st.config.clone();
                self.last_sent = self.draft.clone();
                self.expected = self.draft.clone();
            }
        }
        self.status = status;
    }

    /// Debounced live apply of `draft`.
    fn flush_edits(&mut self, ctx: &egui::Context) {
        if self.draft == self.last_sent {
            self.edited_at = None;
            return;
        }
        let t = *self.edited_at.get_or_insert_with(Instant::now);
        let debounce = Duration::from_millis(120);
        if t.elapsed() < debounce {
            ctx.request_repaint_after(debounce - t.elapsed());
            return;
        }
        self.edited_at = None;
        self.seq += 1;
        self.sent_seq = self.seq;
        self.last_sent = self.draft.clone();
        let mut exp = self.draft.clone();
        exp.sanitize();
        self.expected = exp;
        let _ = self.tx.send(Cmd::Send(self.seq, Request::ApplyConfig(Box::new(self.draft.clone()))));
    }

    fn banner(&mut self, ui: &mut egui::Ui) {
        let (err, offline_saved, conn_err) = {
            let mut s = self.shared.lock().unwrap();
            (s.error.take(), s.saved_offline, s.connect_error.clone())
        };
        if let Some(e) = err {
            self.ui.last_error = Some((e, Instant::now()));
        }
        if !self.connected {
            egui::Frame::group(ui.style()).fill(ui.visuals().warn_fg_color.gamma_multiply(0.15)).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("FreeHM is not running.").strong());
                    ui.label(if offline_saved {
                        "Changes are saved and take effect when it starts."
                    } else {
                        "Changes will be saved to the configuration file and take effect when it starts."
                    });
                    if ui.button("Start FreeHM").clicked() {
                        actions::start_background();
                    }
                });
                if let Some(e) = conn_err {
                    ui.weak(e);
                }
            });
        }
        let rejected = self.rejected();
        if !rejected.is_empty() {
            egui::Frame::group(ui.style()).fill(ui.visuals().error_fg_color.gamma_multiply(0.15)).show(ui, |ui| {
                ui.label(egui::RichText::new("This change was not applied:").strong());
                for r in rejected {
                    ui.label(format!("• {r}"));
                }
            });
        }
        if let Some((e, at)) = &self.ui.last_error {
            if at.elapsed() < Duration::from_secs(8) {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else {
                self.ui.last_error = None;
            }
        }
        self.update_banner(ui);
    }

    /// Shown on every page while there is a newer version.
    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let Some(st) = self.status.clone().filter(|_| self.connected) else { return };
        let u = &st.update;
        let Some(version) = u.latest.as_deref() else { return };
        if !matches!(u.phase, UpdatePhase::Available | UpdatePhase::Downloading | UpdatePhase::WaitingForGame | UpdatePhase::Installing) {
            return;
        }
        egui::Frame::group(ui.style()).fill(pages::GREEN.gamma_multiply(0.15)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                match u.phase {
                    UpdatePhase::Available => {
                        ui.label(egui::RichText::new(format!("FreeHM {version} is available.")).strong());
                        if u.method != UpdateMethod::Manual {
                            let label = if u.message.is_some() { "Try again" } else { "Install now" };
                            if ui.button(label).on_hover_text("Downloads the update and checks it, then installs it. Windows may ask for permission.").clicked()
                            {
                                self.send(Request::InstallUpdate);
                            }
                        }
                    }
                    UpdatePhase::Downloading => {
                        ui.label(format!("Downloading FreeHM {version}…"));
                        ui.add(egui::ProgressBar::new(u.progress.unwrap_or(0.0)).desired_width(180.0).show_percentage());
                    }
                    UpdatePhase::WaitingForGame => {
                        ui.label(format!("FreeHM {version} is ready, and installs when your game closes."));
                    }
                    _ => {
                        ui.label(format!("Installing FreeHM {version}…"));
                    }
                }
                if let Some(url) = &u.page_url {
                    ui.hyperlink_to(if u.method == UpdateMethod::Manual { "Download" } else { "What's new" }, url);
                }
            });
            if let Some(m) = &u.message {
                ui.colored_label(ui.visuals().warn_fg_color, m);
            }
        });
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sync_from_shared();
        // An AppImage restarts as the new version: this window is the old one.
        if self.status.as_ref().is_some_and(|s| s.update.phase == UpdatePhase::Installing && s.update.method == UpdateMethod::AppImage) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.actions.poll(&ctx);
        if self.actions.take_finished() {
            self.send(Request::RefreshPermissions);
            // Record the outcome so the one-time setup is not offered again on its own.
            if let Some((name, Ok(_))) = &self.actions.last {
                self.draft.permissions.state = if name == "install" { PermissionState::Configured } else { PermissionState::NotConfigured };
            }
        }

        egui::Panel::left("nav").resizable(false).exact_size(190.0).show(ui, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.heading("FreeHM");
            });
            ui.add_space(8.0);
            for p in Page::ALL {
                let r = ui.add_sized([ui.available_width(), 30.0], egui::Button::selectable(self.page == p, p.title()));
                if r.clicked() {
                    self.page = p;
                }
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.add_space(8.0);
                let (txt, col) =
                    if self.connected { ("● Running", egui::Color32::from_rgb(118, 185, 0)) } else { ("● Not running", ui.visuals().warn_fg_color) };
                ui.colored_label(col, txt);
            });
        });

        let show_preview = matches!(self.page, Page::General | Page::Overlay | Page::Metrics);
        if show_preview {
            egui::Panel::right("preview").resizable(true).default_size(330.0).min_size(260.0).show(ui, |ui| {
                ui.add_space(10.0);
                ui.heading("Preview");
                let monitors = self.status.as_ref().map(|s| s.monitors.clone()).unwrap_or_default();
                let snap = self.status.as_ref().map(|s| &s.snapshot);
                self.preview.show(ui, &mut self.draft, snap, &monitors);
            });
        }

        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.banner(ui);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.heading(self.page.title());
                ui.add_space(6.0);
                match self.page {
                    Page::General => pages::general(self, ui),
                    Page::Overlay => pages::overlay(self, ui),
                    Page::Metrics => pages::metrics(self, ui),
                    Page::Hardware => pages::hardware(self, ui),
                    Page::Detection => pages::detection(self, ui),
                    Page::Shortcuts => pages::shortcuts(self, ui),
                    Page::Startup => pages::startup(self, ui),
                    Page::About => pages::about(self, ui),
                }
                ui.add_space(20.0);
            });
        });

        if let Some(p) = self.page_request.take() {
            self.page = p;
        }
        // The real overlay is shown while its appearance and position are being edited:
        // on the Overlay page, and for a few seconds after it is moved on any page's map.
        let moved = self.preview.moved_at.is_some_and(|t| t.elapsed() < Duration::from_secs(4));
        if moved {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        let show_overlay = self.page == Page::Overlay || moved;
        let changed = {
            let mut s = self.shared.lock().unwrap();
            std::mem::replace(&mut s.show_overlay, show_overlay) != show_overlay
        };
        if changed {
            self.send(Request::GetStatus { live: true, show_overlay });
        }
        self.flush_edits(&ctx);
    }
}

/// egui's default fonts, with the bundled monospace font (Hack) as a fallback for normal
/// text: the proportional font lacks the status and arrow symbols (● ▲ ▼), and egui 0.37
/// only bundles emoji fonts behind a feature that would add 700 KB.
fn ui_fonts() -> egui::FontDefinitions {
    let mut f = egui::FontDefinitions::default();
    if let Some(p) = f.families.get_mut(&egui::FontFamily::Proportional) {
        p.insert(1, "Hack".to_owned());
    }
    f
}

pub fn run() -> i32 {
    #[cfg(windows)]
    crate::platform::set_dpi_awareness();
    let Some(_instance) = crate::platform::settings_instance() else {
        log::info!("settings window already open");
        return 0;
    };
    let icon = egui::IconData { rgba: crate::icon::icon_rgba(64), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FreeHM Settings")
            .with_app_id("freehm")
            .with_inner_size([1060.0, 700.0])
            .with_min_inner_size([760.0, 480.0])
            .with_icon(icon),
        centered: true,
        ..Default::default()
    };
    let r = eframe::run_native(
        "FreeHM Settings",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            cc.egui_ctx.set_fonts(ui_fonts());
            Ok(Box::new(SettingsApp::new(&cc.egui_ctx)))
        }),
    );
    match r {
        Ok(()) => 0,
        Err(e) => {
            log::error!("settings window failed: {e}");
            actions::report_fatal(&format!("The settings window could not be opened:\n{e}"));
            1
        }
    }
}
