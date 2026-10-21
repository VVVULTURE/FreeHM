//! Live preview: the overlay rendered by the same renderer and layout code the background
//! app uses, plus a map of where it sits on the selected monitor.

use crate::app::compute_placement;
use eframe::egui;
use freehm_core::config::{Config, FontChoice, FontWeight, MonitorChoice};
use freehm_core::ipc::MonitorInfo;
use freehm_core::metrics::Snapshot;
use freehm_core::render::{Renderer, Row, Style, build_rows};
use freehm_core::telemetry::{Engine, MetricSet, PollRequest, mock::MockProvider};
use std::time::{Duration, Instant};

pub struct Preview {
    renderer: Renderer,
    font: (FontChoice, FontWeight),
    pub font_note: Option<String>,
    tex: Option<egui::TextureHandle>,
    tex_px: (u32, u32),
    last: Option<(Vec<Row>, Style)>,
    demo: Engine,
    demo_snap: Snapshot,
    demo_at: Option<Instant>,
    pub light_backdrop: bool,
}

impl Preview {
    pub fn new() -> Preview {
        let font = (FontChoice::Bundled, FontWeight::SemiBold);
        let (renderer, font_note) = Renderer::new(&font.0, font.1);
        Preview {
            renderer,
            font,
            font_note,
            tex: None,
            tex_px: (0, 0),
            last: None,
            demo: Engine::new(vec![Box::new(MockProvider::demo().0)]),
            demo_snap: Snapshot::default(),
            demo_at: None,
            light_backdrop: false,
        }
    }

    fn demo_snapshot(&mut self) -> &Snapshot {
        if self.demo_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(500)) {
            let req = PollRequest { metrics: MetricSet::all(), game_pids: Vec::new() };
            self.demo_snap = self.demo.poll(Instant::now(), &req, Duration::from_millis(500));
            self.demo_at = Some(Instant::now());
        }
        &self.demo_snap
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cfg: &Config, live: Option<&Snapshot>, monitors: &[MonitorInfo]) {
        let want_font = (cfg.overlay.font.clone(), cfg.overlay.font_weight);
        if want_font != self.font {
            let (r, note) = Renderer::new(&want_font.0, want_font.1);
            self.renderer = r;
            self.font_note = note;
            self.font = want_font;
            self.last = None;
        }
        let live = live.filter(|s| !s.devices.is_empty());
        let is_live = live.is_some();
        let snap = match live {
            Some(s) => s.clone(),
            None => {
                ui.ctx().request_repaint_after(Duration::from_millis(500));
                self.demo_snapshot().clone()
            }
        };
        let ppp = ui.ctx().pixels_per_point();
        let rows = build_rows(&snap, cfg);
        let style = Style::from_config(cfg, ppp);
        if self.last.as_ref() != Some(&(rows.clone(), style.clone())) {
            let pm = self.renderer.render(&rows, &style);
            self.tex_px = (pm.width, pm.height);
            if pm.width > 0 {
                // BGRA premultiplied → RGBA premultiplied.
                let mut rgba = pm.data;
                for p in rgba.as_chunks_mut::<4>().0 {
                    p.swap(0, 2);
                }
                let img = egui::ColorImage::from_rgba_premultiplied([pm.width as usize, pm.height as usize], &rgba);
                match &mut self.tex {
                    Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                    None => self.tex = Some(ui.ctx().load_texture("overlay-preview", img, egui::TextureOptions::NEAREST)),
                }
            }
            self.last = Some((rows, style));
        }

        ui.weak(if is_live { "Live values from this PC, at actual size." } else { "Example values (FreeHM is not running), at actual size." });
        ui.checkbox(&mut self.light_backdrop, "Light background");

        // Backdrop standing in for a game scene so the background opacity is visible.
        let (w, h) = (self.tex_px.0 as f32 / ppp, self.tex_px.1 as f32 / ppp);
        let size = egui::vec2(ui.available_width().max(w + 24.0), (h + 32.0).max(140.0));
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let p = ui.painter_at(rect);
        let stripes = if self.light_backdrop {
            [egui::Color32::from_rgb(196, 214, 230), egui::Color32::from_rgb(232, 226, 205), egui::Color32::from_rgb(150, 190, 140)]
        } else {
            [egui::Color32::from_rgb(24, 36, 58), egui::Color32::from_rgb(58, 48, 40), egui::Color32::from_rgb(34, 60, 38)]
        };
        let band = rect.height() / 3.0;
        for (i, c) in stripes.iter().enumerate() {
            let r = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, band * i as f32), egui::vec2(rect.width(), band + 1.0));
            p.rect_filled(r, 0.0, *c);
        }
        if let Some(t) = &self.tex
            && self.tex_px.0 > 0
        {
            let pos = rect.min + egui::vec2(12.0, 16.0);
            let r = egui::Rect::from_min_size(pos, egui::vec2(w, h));
            p.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        } else {
            p.text(rect.center(), egui::Align2::CENTER_CENTER, "No metrics enabled", egui::FontId::proportional(14.0), egui::Color32::WHITE);
        }
        if let Some(n) = &self.font_note {
            ui.colored_label(ui.visuals().warn_fg_color, n);
        }

        // Position map.
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Position").strong());
        let mon = match &cfg.overlay.monitor {
            MonitorChoice::Named(n) => monitors.iter().find(|m| &m.name == n),
            MonitorChoice::Primary => None,
        }
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())
        .cloned()
        .unwrap_or(MonitorInfo { name: "Example 1920×1080".into(), primary: true, x: 0, y: 0, width: 1920, height: 1080, scale: 1.0 });
        // The overlay at the monitor's own scale (it may differ from this window's).
        let (ow, oh) = (self.tex_px.0 as f32 / ppp * mon.scale, self.tex_px.1 as f32 / ppp * mon.scale);
        let place = compute_placement(&mon, cfg, ow.round() as u32, oh.round() as u32);
        let map_w = ui.available_width().min(300.0);
        let k = map_w / mon.width.max(1) as f32;
        let (mrect, _) = ui.allocate_exact_size(egui::vec2(map_w, mon.height as f32 * k), egui::Sense::hover());
        let p = ui.painter_at(mrect);
        p.rect_filled(mrect, 3.0, egui::Color32::from_gray(40));
        p.rect_stroke(mrect, 3.0, egui::Stroke::new(1.0, egui::Color32::from_gray(90)), egui::StrokeKind::Inside);
        let o = egui::Rect::from_min_size(
            mrect.min + egui::vec2((place.x - mon.x) as f32 * k, (place.y - mon.y) as f32 * k),
            egui::vec2((ow * k).max(3.0), (oh * k).max(3.0)),
        );
        p.rect_filled(o, 1.0, egui::Color32::from_rgb(118, 185, 0));
        ui.weak(format!("{} — {}×{} at {:.0} % scale", mon.name, mon.width, mon.height, mon.scale * 100.0));
    }
}
