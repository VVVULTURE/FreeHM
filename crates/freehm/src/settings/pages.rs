//! The settings pages. Each edits `app.draft`; `SettingsApp` applies changes live.

use super::{SettingsApp, actions};
use eframe::egui;
use freehm_core::config::*;
use freehm_core::format::format_reading;
use freehm_core::hotkey::Hotkey;
use freehm_core::ipc::{GameInfo, Request};
use freehm_core::metrics::{DEVICE_GAME, DEVICE_MEMORY, DeviceKind, Freshness, MetricId, Reading, Snapshot};
use std::time::Instant;

static OFL_TEXT: &str = include_str!("../../../../assets/fonts/OFL-Exo2.txt");
static NOTICES: &str = include_str!("../../../../THIRD_PARTY_NOTICES.md");

const GREEN: egui::Color32 = egui::Color32::from_rgb(118, 185, 0);

#[derive(Default)]
pub struct UiState {
    pub last_error: Option<(String, Instant)>,
    new_game: String,
    new_excluded: String,
    font_filter: String,
    recording: bool,
    hotkey_text: Option<String>,
    hotkey_msg: Option<String>,
    confirm_reset: bool,
    /// Last foreground application that was not FreeHM itself (the settings window is
    /// usually in the foreground while this page is read).
    last_foreign_fg: Option<GameInfo>,
    copied_at: Option<Instant>,
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title).strong().size(15.0));
    egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).weak().small()).wrap());
}

fn ok_label(ui: &mut egui::Ui, ok: bool, text: impl Into<String>) {
    let (sym, col) = if ok { ("●", GREEN) } else { ("×", ui.visuals().warn_fg_color) };
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(col, sym);
        ui.label(text.into());
    });
}

fn color_row(ui: &mut egui::Ui, label: &str, c: &mut Rgba) {
    ui.label(label);
    let mut a = [c.0, c.1, c.2, c.3];
    if ui.color_edit_button_srgba_unmultiplied(&mut a).changed() {
        *c = Rgba(a[0], a[1], a[2], a[3]);
    }
    let mut hex = c.to_hex();
    if ui.add(egui::TextEdit::singleline(&mut hex).desired_width(90.0).font(egui::TextStyle::Monospace)).changed()
        && let Some(n) = Rgba::from_hex(&hex)
    {
        *c = n;
    }
    ui.end_row();
}

fn slider(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) {
    ui.label(label);
    ui.add(egui::Slider::new(v, range).suffix(suffix).max_decimals(1));
    ui.end_row();
}

fn weight_name(w: FontWeight) -> &'static str {
    match w {
        FontWeight::Regular => "Regular",
        FontWeight::Medium => "Medium",
        FontWeight::SemiBold => "Semi-bold",
        FontWeight::Bold => "Bold",
    }
}

fn anchor_name(a: Anchor) -> &'static str {
    match a {
        Anchor::TopLeft => "Top left",
        Anchor::TopCenter => "Top center",
        Anchor::TopRight => "Top right",
        Anchor::BottomLeft => "Bottom left",
        Anchor::BottomCenter => "Bottom center",
        Anchor::BottomRight => "Bottom right",
        Anchor::Custom => "Custom",
    }
}

/// The reading the overlay would show for `id`, honouring the device selection.
fn reading_for<'a>(snap: &'a Snapshot, cfg: &Config, id: MetricId) -> Option<&'a Reading> {
    let dev = match id.source() {
        DeviceKind::Cpu => snap.device_for(DeviceKind::Cpu, cfg.hardware.cpu_device.as_deref())?.id.clone(),
        DeviceKind::Gpu => snap.device_for(DeviceKind::Gpu, cfg.hardware.gpu_device.as_deref())?.id.clone(),
        DeviceKind::Memory => DEVICE_MEMORY.to_string(),
        DeviceKind::Game => DEVICE_GAME.to_string(),
    };
    snap.get(&dev, id)
}

fn reading_text(r: &Reading, cfg: &Config) -> (String, bool) {
    let f = format_reading(r, cfg, None);
    if f.available {
        let stale = if r.freshness == Freshness::Stale { " (stale)" } else { "" };
        (format!("{} {}{stale}", f.value, f.unit).trim().to_string(), true)
    } else {
        (format!("N/A — {}", r.reason.as_deref().unwrap_or("not available")), false)
    }
}

// ---------------------------------------------------------------------------------------

pub fn general(app: &mut SettingsApp, ui: &mut egui::Ui) {
    if app.draft.permissions.state == PermissionState::NotConfigured && needs_setup(app) {
        egui::Frame::group(ui.style()).fill(GREEN.gamma_multiply(0.12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("One-time setup").strong());
            hint(ui, SETUP_SUMMARY);
            ui.horizontal(|ui| {
                if ui.button("Open Startup & permissions").clicked() {
                    app.page_request = Some(super::Page::Startup);
                }
                if ui.button("Not now").on_hover_text("FreeHM will not ask again; the setup stays available on the Startup & permissions page.").clicked() {
                    app.draft.permissions.state = PermissionState::Declined;
                }
            });
        });
    }

    section(ui, "Overlay", |ui| {
        if let Some(st) = &app.status {
            ok_label(ui, st.overlay.visible, if st.overlay.visible { format!("Visible — {}", st.overlay.reason) } else { st.overlay.reason.clone() });
            if let Some(w) = &st.overlay.warning {
                ui.colored_label(ui.visuals().warn_fg_color, w);
            }
            if let Some(u) = &st.overlay.unsupported {
                ui.colored_label(ui.visuals().error_fg_color, u);
            }
        }
        ui.horizontal(|ui| {
            let enabled = app.connected;
            if ui.add_enabled(enabled, egui::Button::new("Show now")).clicked() {
                app.send(Request::SetOverlayVisible(true));
            }
            if ui.add_enabled(enabled, egui::Button::new("Hide now")).clicked() {
                app.send(Request::SetOverlayVisible(false));
            }
            ui.weak(format!("or press {}", app.draft.hotkeys.toggle_overlay));
        });
        hint(ui, "A manual show/hide lasts until a different game becomes active.");
        ui.checkbox(&mut app.draft.overlay.show_on_desktop, "Show the overlay on the desktop when no game is running");
    });

    section(ui, "Update interval", |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut app.draft.general.refresh_interval_ms).range(MIN_REFRESH_MS..=MAX_REFRESH_MS).speed(5).suffix(" ms"));
            for v in [100u32, 200, 500, 1000, 2000] {
                if ui.selectable_label(app.draft.general.refresh_interval_ms == v, format!("{v} ms")).clicked() {
                    app.draft.general.refresh_interval_ms = v;
                }
            }
        });
        hint(
            ui,
            &format!(
                "How often sensors are read and the overlay is redrawn ({MIN_REFRESH_MS}–{MAX_REFRESH_MS} ms, default {DEFAULT_REFRESH_MS} ms). \
                 Shorter intervals react faster and cost slightly more CPU. Sensors are not read at all while the overlay is hidden."
            ),
        );
    });

    section(ui, "Units", |ui| {
        ui.horizontal(|ui| {
            ui.label("Temperature:");
            ui.radio_value(&mut app.draft.general.temperature_unit, TemperatureUnit::Celsius, "°C");
            ui.radio_value(&mut app.draft.general.temperature_unit, TemperatureUnit::Fahrenheit, "°F");
        });
        ui.checkbox(&mut app.draft.general.clock_in_ghz, "Show clock speeds in GHz instead of MHz");
    });

    section(ui, "Restore defaults", |ui| {
        if !app.ui.confirm_reset {
            if ui.button("Restore all settings to defaults…").clicked() {
                app.ui.confirm_reset = true;
            }
        } else {
            ui.label("Reset appearance, metrics, detection lists, shortcut and update interval?");
            ui.horizontal(|ui| {
                if ui.button("Reset everything").clicked() {
                    let keep = app.draft.permissions.clone();
                    app.draft = Config { permissions: keep, ..Config::default() };
                    app.ui.confirm_reset = false;
                }
                if ui.button("Cancel").clicked() {
                    app.ui.confirm_reset = false;
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------------------

pub fn overlay(app: &mut SettingsApp, ui: &mut egui::Ui) {
    let o = &mut app.draft.overlay;
    section(ui, "Font", |ui| {
        let current = match &o.font {
            FontChoice::Bundled => "Exo 2 (bundled)".to_string(),
            FontChoice::System { family, .. } => family.clone(),
            FontChoice::File { path } => std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone()),
        };
        egui::Grid::new("font").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Source");
            ui.horizontal(|ui| {
                if ui.selectable_label(o.font == FontChoice::Bundled, "Bundled (Exo 2)").clicked() {
                    o.font = FontChoice::Bundled;
                }
                if ui.selectable_label(matches!(o.font, FontChoice::System { .. }), "Installed font").clicked() && !matches!(o.font, FontChoice::System { .. })
                {
                    o.font = FontChoice::System { family: String::new(), path: None };
                }
                if ui.selectable_label(matches!(o.font, FontChoice::File { .. }), "Font file").clicked() && !matches!(o.font, FontChoice::File { .. }) {
                    o.font = FontChoice::File { path: String::new() };
                }
            });
            ui.end_row();
            ui.label("Current");
            ui.label(current);
            ui.end_row();
            ui.label("Weight");
            let before = o.font_weight;
            egui::ComboBox::from_id_salt("weight").selected_text(weight_name(o.font_weight)).show_ui(ui, |ui| {
                for w in [FontWeight::Regular, FontWeight::Medium, FontWeight::SemiBold, FontWeight::Bold] {
                    ui.selectable_value(&mut o.font_weight, w, weight_name(w));
                }
            });
            ui.end_row();
            if before != o.font_weight
                && let FontChoice::System { family, path } = &mut o.font
                && let Some(f) = app.fonts.lock().unwrap().as_ref()
            {
                *path = f.resolve(family, o.font_weight);
            }
            ui.label("Size");
            ui.add(egui::Slider::new(&mut o.font_size, 8.0..=48.0).suffix(" px").max_decimals(1));
            ui.end_row();
        });
        match &mut o.font {
            FontChoice::Bundled => hint(
                ui,
                "The NVIDIA overlay uses NVIDIA Sans, whose license does not allow redistribution, so FreeHM bundles Exo 2 \
                 (SIL Open Font License 1.1), the closest freely licensed match. If NVIDIA Sans is installed on this PC \
                 (it ships with the NVIDIA App), you can pick it under “Installed font”.",
            ),
            FontChoice::System { family, path } => {
                let fonts = app.fonts.lock().unwrap();
                match fonts.as_ref() {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Reading installed fonts…");
                        });
                    }
                    Some(f) => {
                        ui.add(egui::TextEdit::singleline(&mut app.ui.font_filter).hint_text("Search installed fonts").desired_width(260.0));
                        let filt = app.ui.font_filter.to_lowercase();
                        egui::ScrollArea::vertical().id_salt("fontlist").max_height(180.0).show(ui, |ui| {
                            for fam in f.families.iter().filter(|n| filt.is_empty() || n.to_lowercase().contains(&filt)) {
                                if ui.selectable_label(family == fam, fam).clicked() {
                                    *family = fam.clone();
                                    *path = f.resolve(fam, o.font_weight);
                                }
                            }
                        });
                        if !family.is_empty() && path.is_none() {
                            ui.colored_label(ui.visuals().warn_fg_color, "No usable file was found for this font; the bundled font is used.");
                        }
                    }
                }
            }
            FontChoice::File { path } => {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(path).hint_text("C:\\path\\to\\font.ttf").desired_width(320.0));
                    if ui.button("Browse…").clicked()
                        && let Some(p) = actions::browse_file("font")
                    {
                        *path = p;
                    }
                });
                hint(ui, "TrueType/OpenType file (.ttf, .otf; .ttc with #index). Characters the font lacks are drawn with the bundled font.");
            }
        }
    });

    section(ui, "Colors", |ui| {
        egui::Grid::new("colors").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
            color_row(ui, "Labels", &mut o.label_color);
            color_row(ui, "Values", &mut o.value_color);
            color_row(ui, "Units", &mut o.unit_color);
            color_row(ui, "Background", &mut o.background);
            ui.label("Overall opacity");
            let mut pct = o.opacity * 100.0;
            if ui.add(egui::Slider::new(&mut pct, 10.0..=100.0).suffix(" %").max_decimals(0)).changed() {
                o.opacity = pct / 100.0;
            }
            ui.end_row();
        });
        hint(ui, "The background color's alpha sets its transparency; overall opacity fades text and background together.");
        ui.checkbox(&mut o.text_shadow, "Text shadow (keeps text readable on bright scenes)");
    });

    section(ui, "Layout", |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut o.layout, LayoutMode::Stacked, "One metric per line");
            ui.radio_value(&mut o.layout, LayoutMode::Grouped, "Grouped by device");
            ui.radio_value(&mut o.layout, LayoutMode::SingleLine, "Single line");
        });
        ui.checkbox(&mut o.compact_labels, "Compact labels (CPU, GPU, FT …)");
        ui.checkbox(&mut o.show_units, "Show units");
        ui.collapsing("Spacing", |ui| {
            egui::Grid::new("spacing").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                slider(ui, "Padding", &mut o.padding, 0.0..=40.0, " px");
                slider(ui, "Line spacing", &mut o.line_spacing, 0.0..=24.0, " px");
                slider(ui, "Column gap", &mut o.column_gap, 0.0..=64.0, " px");
                slider(ui, "Corner radius", &mut o.corner_radius, 0.0..=24.0, " px");
            });
        });
    });

    let monitors = app.status.as_ref().map(|s| s.monitors.clone()).unwrap_or_default();
    section(ui, "Size and position", |ui| {
        egui::Grid::new("pos").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Scale");
            let mut pct = o.scale * 100.0;
            if ui.add(egui::Slider::new(&mut pct, 50.0..=300.0).suffix(" %").max_decimals(0)).changed() {
                o.scale = pct / 100.0;
            }
            ui.end_row();
            ui.label("Monitor");
            let sel = match &o.monitor {
                MonitorChoice::Primary => "Primary monitor".to_string(),
                MonitorChoice::Named(n) => {
                    if monitors.iter().any(|m| &m.name == n) {
                        n.clone()
                    } else {
                        format!("{n} (not connected — using primary)")
                    }
                }
            };
            egui::ComboBox::from_id_salt("monitor").selected_text(sel).width(260.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut o.monitor, MonitorChoice::Primary, "Primary monitor");
                for m in &monitors {
                    let text = format!("{} — {}×{}{}", m.name, m.width, m.height, if m.primary { " (primary)" } else { "" });
                    ui.selectable_value(&mut o.monitor, MonitorChoice::Named(m.name.clone()), text);
                }
            });
            ui.end_row();
            ui.label("Corner");
            ui.vertical(|ui| {
                for row in [[Anchor::TopLeft, Anchor::TopCenter, Anchor::TopRight], [Anchor::BottomLeft, Anchor::BottomCenter, Anchor::BottomRight]] {
                    ui.horizontal(|ui| {
                        for a in row {
                            if ui.add_sized([100.0, 24.0], egui::Button::selectable(o.anchor == a, anchor_name(a))).clicked() {
                                o.anchor = a;
                            }
                        }
                    });
                }
                if ui.add_sized([100.0, 24.0], egui::Button::selectable(o.anchor == Anchor::Custom, "Custom")).clicked() {
                    o.anchor = Anchor::Custom;
                }
            });
            ui.end_row();
            if o.anchor == Anchor::Custom {
                ui.label("Position (x, y)");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut o.offset_x).range(-16000.0..=16000.0).suffix(" px"));
                    ui.add(egui::DragValue::new(&mut o.offset_y).range(-16000.0..=16000.0).suffix(" px"));
                });
                ui.end_row();
            } else {
                ui.label("Edge margins (x, y)");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut o.margin_x).range(0.0..=4000.0).suffix(" px"));
                    ui.add(egui::DragValue::new(&mut o.margin_y).range(0.0..=4000.0).suffix(" px"));
                });
                ui.end_row();
            }
        });
        hint(
            ui,
            "Sizes are logical pixels at 100 % display scaling; FreeHM follows each monitor's Windows/desktop scaling. The overlay never leaves its monitor.",
        );
    });

    if cfg!(windows) {
        section(ui, "Screen capture", |ui| {
            ui.checkbox(&mut o.hide_from_capture, "Hide the overlay from screenshots and recordings");
            hint(ui, "Uses the Windows display-affinity setting. Some capture tools ignore it.");
        });
    }

    ui.add_space(8.0);
    if ui.button("Restore default appearance").clicked() {
        let keep_desktop = app.draft.overlay.show_on_desktop;
        app.draft.overlay = OverlayConfig { show_on_desktop: keep_desktop, ..OverlayConfig::default() };
    }
}

// ---------------------------------------------------------------------------------------

pub fn metrics(app: &mut SettingsApp, ui: &mut egui::Ui) {
    hint(ui, "Choose what the overlay shows and in which order. Values marked N/A cannot be read on this PC right now; the reason is shown.");
    ui.add_space(4.0);
    let snap = app.status.as_ref().map(|s| s.snapshot.clone());
    let cfg_view = app.draft.clone();
    let grouped = app.draft.overlay.layout == LayoutMode::Grouped;
    let n = app.draft.metrics.len();
    let mut swap: Option<(usize, usize)> = None;
    egui::Grid::new("metrics").num_columns(7).striped(true).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.strong("");
        ui.strong("Metric");
        ui.strong("Label");
        ui.strong(if grouped { "Group" } else { "" });
        ui.strong("");
        ui.strong("Order");
        ui.strong("Now");
        ui.end_row();
        for (i, e) in app.draft.metrics.iter_mut().enumerate() {
            ui.checkbox(&mut e.enabled, "");
            ui.label(e.id.display_name());
            let mut label = e.label.clone().unwrap_or_default();
            let def = if cfg_view.overlay.compact_labels { e.id.compact_label() } else { e.id.default_label() };
            // add_sized: inside a Grid, desired_width alone collapses to the previous frame's column width.
            if ui.add_sized([100.0, ui.spacing().interact_size.y], egui::TextEdit::singleline(&mut label).hint_text(def)).changed() {
                e.label = if label.is_empty() { None } else { Some(label) };
            }
            if grouped {
                let mut g = e.group.clone().unwrap_or_default();
                if ui.add_sized([70.0, ui.spacing().interact_size.y], egui::TextEdit::singleline(&mut g).hint_text(e.id.default_group())).changed() {
                    e.group = if g.is_empty() { None } else { Some(g) };
                }
            } else {
                ui.label("");
            }
            if matches!(e.id, MetricId::RamUsed | MetricId::VramUsed) {
                ui.checkbox(&mut e.show_total, "/ total").on_hover_text("Show “used / total”");
            } else {
                ui.label("");
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(i > 0, egui::Button::new("▲").small()).on_hover_text("Move up").clicked() {
                    swap = Some((i, i - 1));
                }
                if ui.add_enabled(i + 1 < n, egui::Button::new("▼").small()).on_hover_text("Move down").clicked() {
                    swap = Some((i, i + 1));
                }
            });
            match snap.as_ref().and_then(|s| reading_for(s, &cfg_view, e.id)) {
                Some(r) => {
                    let (t, ok) = reading_text(r, &cfg_view);
                    if ok {
                        ui.label(t);
                    } else {
                        ui.add(egui::Label::new(egui::RichText::new(t).color(ui.visuals().warn_fg_color)).truncate())
                            .on_hover_text(r.reason.clone().unwrap_or_default());
                    }
                }
                None => {
                    ui.weak(if snap.is_some() { "no device" } else { "—" });
                }
            }
            ui.end_row();
        }
    });
    if let Some((a, b)) = swap {
        app.draft.metrics.swap(a, b);
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Enable all").clicked() {
            app.draft.metrics.iter_mut().for_each(|m| m.enabled = true);
        }
        if ui.button("Disable all").clicked() {
            app.draft.metrics.iter_mut().for_each(|m| m.enabled = false);
        }
        if ui.button("Reset metrics to defaults").clicked() {
            app.draft.metrics = default_metrics();
        }
    });
    hint(
        ui,
        "Frame time is the average time between presented frames of the active game over the last 250 ms; FPS counts frames over the last second. \
         GPU shared memory is system RAM the GPU is using (integrated GPUs have no dedicated VRAM).",
    );
}

// ---------------------------------------------------------------------------------------

pub fn hardware(app: &mut SettingsApp, ui: &mut egui::Ui) {
    let Some(st) = app.status.clone() else {
        ui.label("Start FreeHM to see the detected hardware and sensor availability.");
        return;
    };
    let snap = &st.snapshot;
    section(ui, "Devices used by the overlay", |ui| {
        egui::Grid::new("devsel").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            for (kind, label) in [(DeviceKind::Cpu, "CPU"), (DeviceKind::Gpu, "GPU")] {
                ui.label(label);
                let sel = match kind {
                    DeviceKind::Cpu => &mut app.draft.hardware.cpu_device,
                    _ => &mut app.draft.hardware.gpu_device,
                };
                let auto = snap.device_for(kind, None).map(|d| d.name.clone()).unwrap_or_else(|| "none found".into());
                let text = match sel.as_ref().and_then(|id| snap.devices.iter().find(|d| &d.id == id)) {
                    Some(d) => d.name.clone(),
                    None if sel.is_some() => "Selected device not present — automatic".into(),
                    None => format!("Automatic ({auto})"),
                };
                egui::ComboBox::from_id_salt(label).selected_text(text).width(340.0).show_ui(ui, |ui| {
                    ui.selectable_value(sel, None, format!("Automatic ({auto})"));
                    for d in snap.devices_of(kind) {
                        let name = if d.integrated { format!("{} (integrated)", d.name) } else { d.name.clone() };
                        ui.selectable_value(sel, Some(d.id.clone()), name);
                    }
                });
                ui.end_row();
            }
        });
        hint(ui, "Automatic prefers a discrete GPU over an integrated one.");
    });

    for d in &snap.devices {
        let title = format!("{}{}", d.name, if d.integrated { " — integrated" } else { "" });
        egui::CollapsingHeader::new(egui::RichText::new(title).strong()).id_salt(&d.id).default_open(d.kind != DeviceKind::Memory).show(ui, |ui| {
            ui.weak(format!("{:?} · {:?} · id {} · via {}", d.kind, d.vendor, d.id, d.provider));
            if d.integrated {
                hint(ui, "Integrated GPU: it has no dedicated VRAM; memory it uses is reported as GPU shared memory (part of system RAM).");
            }
            egui::Grid::new(format!("r-{}", d.id)).num_columns(3).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for r in snap.readings.iter().filter(|r| r.device == d.id) {
                    ui.label(r.metric.display_name());
                    let (t, ok) = reading_text(r, &app.draft);
                    if ok {
                        ui.label(t);
                    } else {
                        ui.colored_label(ui.visuals().warn_fg_color, t);
                    }
                    ui.weak(&r.source);
                    ui.end_row();
                }
            });
        });
    }
    let game: Vec<&Reading> = snap.readings.iter().filter(|r| r.device == DEVICE_GAME).collect();
    if !game.is_empty() {
        // Without a detected game, frame timing follows the foreground application.
        let title = match app.status.as_ref().and_then(|s| s.game.active.as_ref()) {
            Some(g) => format!("Active game: {}", g.name),
            None => "Foreground application (no game detected)".to_string(),
        };
        egui::CollapsingHeader::new(egui::RichText::new(title).strong()).id_salt("active-game").default_open(true).show(ui, |ui| {
            for r in game {
                let (t, ok) = reading_text(r, &app.draft);
                ok_label(ui, ok, format!("{}: {t}", r.metric.display_name()));
            }
        });
    }

    section(ui, "Sensor providers", |ui| {
        for p in &snap.provider_status {
            ok_label(ui, p.ok, format!("{} — {}", p.provider, p.message));
        }
        if snap.provider_status.is_empty() {
            ui.weak("No provider has reported yet.");
        }
    });

    if st.helper.applicable {
        section(ui, "Helper service", |ui| {
            let mut on = app.draft.hardware.use_helper == HelperUse::Enabled;
            if ui.checkbox(&mut on, "Use the FreeHM helper for CPU temperature and FPS / frame time").changed() {
                app.draft.hardware.use_helper = if on { HelperUse::Enabled } else { HelperUse::Disabled };
            }
            hint(
                ui,
                "Turn this off before playing a game whose anti-cheat objects to hardware-access drivers: FreeHM then never contacts the helper, and the helper unloads its sensor driver module.",
            );
            ok_label(ui, st.helper.connected, st.helper.message.clone());
        });
    }
}

// ---------------------------------------------------------------------------------------

fn list_editor(ui: &mut egui::Ui, id: &str, list: &mut Vec<String>, new: &mut String, placeholder: &str) {
    let mut remove = None;
    for (i, item) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui.small_button("×").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
            ui.label(item);
        });
    }
    if list.is_empty() {
        ui.weak("(empty)");
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(new).hint_text(placeholder).desired_width(260.0).id_salt(id));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("Add").clicked() || enter) && !new.trim().is_empty() {
            push_unique(list, new.trim());
            new.clear();
        }
        if ui.button("Browse…").clicked()
            && let Some(p) = actions::browse_file("exe")
        {
            push_unique(list, &p);
        }
    });
}

fn push_unique(list: &mut Vec<String>, s: &str) {
    if !list.iter().any(|x| x.eq_ignore_ascii_case(s)) {
        list.push(s.to_string());
    }
}

fn is_freehm(g: &GameInfo) -> bool {
    let n = g.name.to_ascii_lowercase();
    n == "freehm.exe" || n == "freehm"
}

pub fn detection(app: &mut SettingsApp, ui: &mut egui::Ui) {
    if let Some(fg) = app.status.as_ref().and_then(|s| s.game.foreground.clone())
        && !is_freehm(&fg)
    {
        app.ui.last_foreign_fg = Some(fg);
    }
    let d = &mut app.draft.detection;
    section(ui, "Automatic detection", |ui| {
        ui.checkbox(&mut d.auto_detect, "Detect games automatically");
        ui.horizontal(|ui| {
            ui.label("Show the overlay for:");
            ui.radio_value(&mut d.show_for, ShowFor::AllDetectedGames, "all detected games");
            ui.radio_value(&mut d.show_for, ShowFor::SelectedGamesOnly, "only games in my list");
        });
        ui.checkbox(&mut d.detect_fullscreen_apps, "Treat unknown full-screen applications as games");
        ui.checkbox(&mut app.draft.overlay.show_on_desktop, "Show the overlay on the desktop when no game is running");
        ui.horizontal(|ui| {
            ui.label("Check every");
            ui.add(egui::DragValue::new(&mut d.interval_ms).range(500..=30_000).speed(50).suffix(" ms"));
            ui.weak("(foreground changes are noticed immediately on Windows)");
        });
        hint(
            ui,
            "Detection uses only local information: the foreground application, its process tree, and the install folders of \
             Steam, Epic, GOG, Battle.net, EA, Ubisoft Connect, Xbox/Microsoft Store and itch.io games. Launchers themselves never count as games. \
             Nothing is injected into or read from game processes.",
        );
    });

    section(ui, "Right now", |ui| {
        match app.status.as_ref() {
            None => {
                ui.weak("FreeHM is not running.");
            }
            Some(st) => match &st.game.active {
                Some(g) => {
                    ok_label(ui, true, format!("Game: {} (PID {}) — {}", g.name, g.pid, g.reason));
                    if ui.button(format!("Never show the overlay for {}", g.name)).clicked() {
                        push_unique(&mut d.excluded, &g.name);
                    }
                }
                None => ok_label(ui, false, "No game detected"),
            },
        }
        if let Some(fg) = app.ui.last_foreign_fg.clone() {
            ui.add_space(4.0);
            ui.label(format!("Last application in the foreground: {} — {}", fg.name, if fg.is_game { "game" } else { "not a game" }));
            hint(ui, &format!("{} · {}", fg.reason, fg.path));
            ui.horizontal(|ui| {
                if ui.button("Treat as a game").clicked() {
                    push_unique(&mut d.manual_games, &fg.name);
                    d.excluded.retain(|x| !x.eq_ignore_ascii_case(&fg.name));
                }
                if ui.button("Never treat as a game").clicked() {
                    push_unique(&mut d.excluded, &fg.name);
                    d.manual_games.retain(|x| !x.eq_ignore_ascii_case(&fg.name));
                }
            });
        } else {
            hint(ui, "Switch to a game or application and back to see how it was classified.");
        }
    });

    section(ui, "My games (always treated as games)", |ui| {
        list_editor(ui, "games", &mut d.manual_games, &mut app.ui.new_game, "game.exe or full path");
    });
    section(ui, "Never treat as games", |ui| {
        list_editor(ui, "excl", &mut d.excluded, &mut app.ui.new_excluded, "app.exe or full path");
        hint(ui, "Exclusions win over everything else, including your games list.");
    });
}

// ---------------------------------------------------------------------------------------

fn egui_key_name(k: egui::Key) -> Option<String> {
    use egui::Key as K;
    let s = match k {
        K::Insert => "Insert",
        K::Delete => "Delete",
        K::Home => "Home",
        K::End => "End",
        K::PageUp => "PageUp",
        K::PageDown => "PageDown",
        K::Pause => "Pause",
        K::ScrollLock => "ScrollLock",
        K::PrintScreen => "PrintScreen",
        K::Backtick => "`",
        _ => {
            let n = k.name();
            let ok = (n.len() == 1 && n.chars().all(|c| c.is_ascii_alphanumeric()))
                || (n.starts_with('F') && n[1..].parse::<u8>().is_ok_and(|v| (1..=24).contains(&v)));
            return ok.then(|| n.to_string());
        }
    };
    Some(s.to_string())
}

pub fn shortcuts(app: &mut SettingsApp, ui: &mut egui::Ui) {
    section(ui, "Show / hide overlay", |ui| {
        ui.horizontal(|ui| {
            ui.label("Shortcut:");
            ui.label(egui::RichText::new(&app.draft.hotkeys.toggle_overlay).monospace().strong().size(16.0));
        });
        if let Some(st) = &app.status {
            let h = &st.hotkey;
            if h.registered {
                ok_label(ui, true, format!("Active ({})", h.mechanism));
            } else {
                ok_label(ui, false, h.error.clone().unwrap_or_else(|| "not registered".into()));
            }
        }

        if app.ui.recording {
            ui.colored_label(GREEN, "Press the new combination… (Esc cancels)");
            let mut done = None;
            ui.input(|i| {
                for ev in &i.events {
                    if let egui::Event::Key { key, pressed: true, modifiers, .. } = ev {
                        if *key == egui::Key::Escape && modifiers.is_none() {
                            done = Some(None);
                            continue;
                        }
                        if let Some(name) = egui_key_name(*key) {
                            let mut s = String::new();
                            if modifiers.ctrl {
                                s += "Ctrl+";
                            }
                            if modifiers.shift {
                                s += "Shift+";
                            }
                            if modifiers.alt {
                                s += "Alt+";
                            }
                            s += &name;
                            done = Some(Some(s));
                        }
                    }
                }
            });
            match done {
                Some(Some(s)) => {
                    app.ui.recording = false;
                    match Hotkey::parse(&s) {
                        Ok(hk) => {
                            app.draft.hotkeys.toggle_overlay = hk.to_string();
                            app.ui.hotkey_msg = None;
                        }
                        Err(e) => app.ui.hotkey_msg = Some(format!("{s}: {e}")),
                    }
                }
                Some(None) => app.ui.recording = false,
                None => ui.ctx().request_repaint(),
            }
        } else {
            ui.horizontal(|ui| {
                if ui.button("Record a new shortcut").clicked() {
                    app.ui.recording = true;
                    app.ui.hotkey_msg = None;
                }
                if ui.button(format!("Reset to {DEFAULT_HOTKEY}")).clicked() {
                    app.draft.hotkeys.toggle_overlay = DEFAULT_HOTKEY.into();
                    app.ui.hotkey_msg = None;
                }
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Or type it:");
            let text = app.ui.hotkey_text.get_or_insert_with(|| app.draft.hotkeys.toggle_overlay.clone());
            let r = ui.add(egui::TextEdit::singleline(text).desired_width(180.0).hint_text("e.g. Ctrl+Alt+F10"));
            let submit = ui.button("Use").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if submit {
                match Hotkey::parse(text) {
                    Ok(hk) => {
                        app.draft.hotkeys.toggle_overlay = hk.to_string();
                        app.ui.hotkey_msg = None;
                        app.ui.hotkey_text = None;
                    }
                    Err(e) => app.ui.hotkey_msg = Some(e),
                }
            }
        });
        if let Some(m) = &app.ui.hotkey_msg {
            ui.colored_label(ui.visuals().error_fg_color, m);
        }
        hint(
            ui,
            "Letters and digits need Ctrl, Alt or Win; function keys may be used alone. The Win key cannot be recorded here — type it (Win+F9). \
             If another program already owns the combination, FreeHM says so above; choose a different one. \
             While recording, the current shortcut still toggles the overlay.",
        );
        if cfg!(target_os = "linux") {
            hint(
                ui,
                "Wayland desktops do not let applications grab keys. FreeHM uses the desktop's Global Shortcuts portal where available; \
                 otherwise bind a shortcut in your desktop's keyboard settings to the command `freehm --toggle`.",
            );
        }
    });
}

// ---------------------------------------------------------------------------------------

const SETUP_SUMMARY: &str = "CPU temperature, and on Windows FPS / frame time, need a small helper that runs with system rights. \
     Installing it asks for administrator approval once; afterwards FreeHM never asks again.";

fn needs_setup(app: &SettingsApp) -> bool {
    match &app.status {
        Some(st) if st.helper.applicable => !st.helper.installed,
        Some(_) => cfg!(target_os = "linux") && app.draft.permissions.state != PermissionState::Configured,
        None => false,
    }
}

pub fn startup(app: &mut SettingsApp, ui: &mut egui::Ui) {
    section(ui, "Start with Windows / desktop session", |ui| {
        ui.checkbox(&mut app.draft.general.start_with_system, "Start FreeHM automatically when I sign in");
        if let Some(st) = &app.status {
            let a = &st.autostart;
            if let Some(e) = &a.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            } else {
                ui.weak(format!("{} — {}", if a.enabled { "enabled" } else { "disabled" }, a.mechanism));
            }
        }
        hint(ui, "FreeHM starts hidden in the notification area. This is a per-user setting and needs no administrator rights.");
    });

    let ctx = ui.ctx().clone();
    let helper = app.status.as_ref().map(|s| s.helper.clone());
    if cfg!(windows) {
        section(ui, "Hardware access helper (Windows)", |ui| {
            hint(ui, SETUP_SUMMARY);
            hint(
                ui,
                "The helper (service “FreeHMHelper”) reads CPU temperature through the signed PawnIO driver and frame timing from \
                 Windows' own graphics event tracing. It accepts only one fixed, read-only request from local users, never touches \
                 game processes, and can be removed here at any time.",
            );
            match &helper {
                Some(h) => {
                    ok_label(
                        ui,
                        h.pawnio_installed,
                        if h.pawnio_installed { "PawnIO driver installed" } else { "PawnIO driver not installed (needed for CPU temperature only)" },
                    );
                    if !h.pawnio_installed {
                        ui.horizontal(|ui| {
                            ui.label("Install it with:");
                            ui.code("winget install namazso.PawnIO");
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text("winget install namazso.PawnIO".into());
                            }
                            ui.hyperlink_to("pawnio.eu", "https://pawnio.eu");
                        });
                    }
                    ok_label(ui, h.installed, if h.installed { "Helper installed" } else { "Helper not installed" });
                    if h.installed {
                        ok_label(ui, h.running, if h.running { "Helper running" } else { "Helper not running" });
                    }
                    ui.weak(&h.message);
                }
                None => {
                    ui.weak("Start FreeHM to see the helper status.");
                }
            }
            #[cfg(windows)]
            {
                let installed = helper.as_ref().is_some_and(|h| h.installed);
                ui.horizontal(|ui| {
                    let busy = app.actions.busy().is_some();
                    let label = if installed { "Repair helper" } else { "Install helper" };
                    if ui.add_enabled(!busy, egui::Button::new(label)).on_hover_text("Asks for administrator approval once").clicked() {
                        app.actions.spawn("install", &ctx, || actions::helper_command("install"));
                    }
                    if installed && ui.add_enabled(!busy, egui::Button::new("Uninstall helper")).clicked() {
                        app.actions.spawn("uninstall", &ctx, || actions::helper_command("uninstall"));
                    }
                    if busy {
                        ui.spinner();
                        ui.label("Waiting for the administrator prompt…");
                    }
                });
            }
        });
    }

    #[cfg(target_os = "linux")]
    section(ui, "Sensor permissions (Linux)", |ui| {
        hint(
            ui,
            "Most sensors are readable by any user. CPU package power (RAPL) is root-only on current kernels; the one-time setup \
             adds a udev rule that makes those energy counters readable by the “freehm” group and adds you to it. \
             It asks for your password once through polkit. Log out and back in afterwards.",
        );
        ui.weak(format!("State: {:?}", app.draft.permissions.state));
        ui.horizontal(|ui| {
            let busy = app.actions.busy().is_some();
            if ui.add_enabled(!busy, egui::Button::new("Set up sensor access")).clicked() {
                app.actions.spawn("install", &ctx, || actions::linux_permissions(false));
            }
            if ui.add_enabled(!busy, egui::Button::new("Remove")).clicked() {
                app.actions.spawn("uninstall", &ctx, || actions::linux_permissions(true));
            }
            if busy {
                ui.spinner();
            }
        });
    });

    if let Some((name, r)) = &app.actions.last {
        match r {
            Ok(m) => {
                ui.colored_label(GREEN, format!("● {name}: {m}"));
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("× {name}: {e}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------

fn diagnostics_text(app: &SettingsApp) -> String {
    let mut s = format!("FreeHM {} diagnostics\n", freehm_core::APP_VERSION);
    match &app.status {
        Some(st) => s += &serde_json::to_string_pretty(&**st).unwrap_or_default(),
        None => s += "background app not running\n",
    }
    let log = std::fs::read_to_string(crate::paths::log_file()).unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(200).collect();
    s += "\n\n--- log (last 200 lines) ---\n";
    for l in tail.into_iter().rev() {
        s += l;
        s.push('\n');
    }
    s
}

pub fn about(app: &mut SettingsApp, ui: &mut egui::Ui) {
    section(ui, "FreeHM", |ui| {
        ui.label(format!("Version {}", freehm_core::APP_VERSION));
        ui.label("Free, open-source hardware statistics overlay. MIT License.");
        ui.hyperlink_to("github.com/VVVULTURE/FreeHM", "https://github.com/VVVULTURE/FreeHM");
        hint(ui, "FreeHM never connects to the network, has no account and sends no telemetry.");
        ui.horizontal(|ui| {
            if app.connected {
                if ui.button("Quit FreeHM").on_hover_text("Stops monitoring and removes the tray icon").clicked() {
                    app.send(Request::Quit);
                }
            } else if ui.button("Start FreeHM").clicked() {
                actions::start_background();
            }
        });
    });

    section(ui, "Diagnostics", |ui| {
        if let Some(st) = app.status.clone() {
            egui::Grid::new("diag").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Platform");
                ui.label(&st.platform);
                ui.end_row();
                ui.label("Overlay");
                ui.label(&st.overlay.backend);
                ui.end_row();
                ui.label("Overlay state");
                ui.label(&st.overlay.reason);
                ui.end_row();
                ui.label("Shortcut");
                ui.label(if st.hotkey.registered {
                    format!("{} via {}", st.hotkey.shortcut, st.hotkey.mechanism)
                } else {
                    st.hotkey.error.clone().unwrap_or_default()
                });
                ui.end_row();
                ui.label("Monitors");
                ui.label(st.monitors.iter().map(|m| format!("{} {}×{} @{:.0}%", m.name, m.width, m.height, m.scale * 100.0)).collect::<Vec<_>>().join(", "));
                ui.end_row();
            });
            for n in &st.notes {
                ui.colored_label(ui.visuals().warn_fg_color, n);
            }
        }
        let cfg_path = crate::paths::config_file();
        let log_path = crate::paths::log_file();
        for (label, p) in [("Settings file", cfg_path), ("Log file", log_path)] {
            ui.horizontal(|ui| {
                ui.label(format!("{label}:"));
                ui.code(p.display().to_string());
                if ui.small_button("Open folder").clicked()
                    && let Some(dir) = p.parent()
                {
                    actions::open_path(dir);
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.button("Copy diagnostics to clipboard").clicked() {
                ui.ctx().copy_text(diagnostics_text(app));
                app.ui.copied_at = Some(Instant::now());
            }
            if app.ui.copied_at.is_some_and(|t| t.elapsed().as_secs() < 3) {
                ui.colored_label(GREEN, "Copied");
            }
        });
        hint(
            ui,
            "Diagnostics contain your settings, detected hardware, recent log lines and the name/path of the foreground application. Review before sharing.",
        );
    });

    section(ui, "Licenses", |ui| {
        ui.collapsing("Third-party notices", |ui| {
            egui::ScrollArea::vertical().id_salt("notices").max_height(260.0).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(NOTICES).monospace().small()).wrap());
            });
        });
        ui.collapsing("Exo 2 font — SIL Open Font License 1.1", |ui| {
            egui::ScrollArea::vertical().id_salt("ofl").max_height(260.0).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(OFL_TEXT).monospace().small()).wrap());
            });
        });
    });
}
