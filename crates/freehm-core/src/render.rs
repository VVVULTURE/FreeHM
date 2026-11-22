//! Overlay layout and software rendering.
//!
//! The overlay is a few hundred pixels of text, redrawn at most once per refresh and only
//! when its text changes, so a CPU rasterizer is cheaper than holding a GPU context open
//! for the lifetime of the process. The output is a premultiplied BGRA pixmap, which is
//! the native format of Windows layered windows (`UpdateLayeredWindow`), X11 ARGB32
//! visuals and Wayland `ARGB8888` shm buffers on little-endian machines.

use crate::config::{Config, DisplayMode, FontChoice, FontWeight, LayoutMode, Rgba};
use crate::format::{Formatted, format_reading};
use crate::metrics::{DEVICE_GAME, DEVICE_MEMORY, DeviceKind, MetricId, Snapshot};
use std::collections::HashMap;

static EXO2_REGULAR: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/Exo2-Regular.ttf"));
static EXO2_MEDIUM: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/Exo2-Medium.ttf"));
static EXO2_SEMIBOLD: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/Exo2-SemiBold.ttf"));
static EXO2_BOLD: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/Exo2-Bold.ttf"));

pub const BUNDLED_FONT_NAME: &str = "Exo 2 (bundled, SIL OFL 1.1)";

pub fn bundled_font_bytes(weight: FontWeight) -> &'static [u8] {
    match weight {
        FontWeight::Regular => EXO2_REGULAR,
        FontWeight::Medium => EXO2_MEDIUM,
        FontWeight::SemiBold => EXO2_SEMIBOLD,
        FontWeight::Bold => EXO2_BOLD,
    }
}

/// Premultiplied BGRA image, rows top to bottom, no padding.
#[derive(Clone, PartialEq, Eq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Pixmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pixmap({}x{})", self.width, self.height)
    }
}

impl Pixmap {
    pub fn new(width: u32, height: u32) -> Pixmap {
        Pixmap { width, height, data: vec![0; (width * height * 4) as usize] }
    }
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
    /// Un-premultiplied RGBA copy (for image viewers / egui textures).
    pub fn to_rgba_unpremultiplied(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len());
        for p in self.data.as_chunks::<4>().0 {
            let a = p[3] as u32;
            let un = |c: u8| (c as u32 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
            out.extend_from_slice(&[un(p[2]), un(p[1]), un(p[0]), p[3]]);
        }
        out
    }
}

/// One overlay line, already formatted.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Leading label column (Box Mode); `None` when the label is hidden or in Pancake Mode.
    pub label: Option<String>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Label drawn just before the value (Line / Pancake Mode).
    pub prefix: Option<String>,
    pub value: String,
    pub unit: String,
    pub dim: bool,
}

/// Build the formatted rows for the enabled metrics in configured order.
pub fn build_rows(snap: &Snapshot, cfg: &Config) -> Vec<Row> {
    let cpu = snap.device_for(DeviceKind::Cpu, cfg.hardware.cpu_device.as_deref()).map(|d| d.id.clone());
    let gpu = snap.device_for(DeviceKind::Gpu, cfg.hardware.gpu_device.as_deref()).map(|d| d.id.clone());
    let device_of = |m: MetricId| -> Option<String> {
        match m.source() {
            DeviceKind::Cpu => cpu.clone(),
            DeviceKind::Gpu => gpu.clone(),
            DeviceKind::Memory => Some(DEVICE_MEMORY.to_string()),
            DeviceKind::Game => Some(DEVICE_GAME.to_string()),
        }
    };
    let o = &cfg.overlay;
    let compact = o.compact_labels;

    // (label, group, value); the label is `None` when it is switched off.
    let mut entries: Vec<(Option<String>, String, Formatted)> = Vec::new();
    for e in cfg.enabled_metrics() {
        let label = (o.show_labels && e.show_label)
            .then(|| e.label.clone().unwrap_or_else(|| if compact { e.id.compact_label() } else { e.id.default_label() }.to_string()));
        let group = e.group.clone().unwrap_or_else(|| e.id.default_group().to_string());
        let f = match device_of(e.id).and_then(|d| snap.get(&d, e.id).cloned().map(|r| (d, r))) {
            Some((dev, r)) => {
                let total_metric = match e.id {
                    MetricId::RamUsed if e.show_total => Some(MetricId::RamTotal),
                    MetricId::VramUsed if e.show_total => Some(MetricId::VramTotal),
                    _ => None,
                };
                let total = total_metric.and_then(|t| snap.get(&dev, t)).filter(|t| t.is_available());
                format_reading(&r, cfg, total)
            }
            None => Formatted { value: crate::format::UNAVAILABLE.into(), unit: String::new(), dim: true, available: false },
        };
        let f = if o.show_units { f } else { Formatted { unit: String::new(), ..f } };
        entries.push((label, group, f));
    }

    let item = |prefix: Option<String>, f: Formatted| Item { prefix, value: f.value, unit: f.unit, dim: f.dim };
    match (o.display_mode, o.layout) {
        (DisplayMode::Pancake, _) => {
            let n = entries.len();
            if n == 0 {
                return Vec::new();
            }
            // More rows than statistics leaves one statistic per row.
            let rows = (o.pancake_rows.max(1) as usize).min(n);
            // As even as possible, the top rows taking the extra ones: 7 in 2 rows is 4 + 3.
            let (base, extra) = (n / rows, n % rows);
            let mut left = entries.into_iter();
            (0..rows).map(|r| Row { label: None, items: left.by_ref().take(base + usize::from(r < extra)).map(|(l, _, f)| item(l, f)).collect() }).collect()
        }
        (DisplayMode::Box, LayoutMode::Stacked) => entries.into_iter().map(|(label, _, f)| Row { label, items: vec![item(None, f)] }).collect(),
        (DisplayMode::Box, LayoutMode::Grouped) => {
            // Each line is labelled by its device, so only the main label switch applies.
            let mut rows: Vec<(String, Row)> = Vec::new();
            for (_, group, f) in entries {
                match rows.last_mut() {
                    Some((g, row)) if *g == group => row.items.push(item(None, f)),
                    _ => rows.push((group.clone(), Row { label: o.show_labels.then_some(group), items: vec![item(None, f)] })),
                }
            }
            rows.into_iter().map(|(_, r)| r).collect()
        }
    }
}

/// Visual parameters resolved for one render (already multiplied by DPI and user scale).
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub px: f32,
    pub padding: f32,
    pub line_spacing: f32,
    pub column_gap: f32,
    pub corner_radius: f32,
    pub label: Rgba,
    pub value: Rgba,
    pub unit: Rgba,
    pub background: Rgba,
    pub opacity: f32,
    pub shadow: bool,
    pub columns: Columns,
}

/// How the items of the rows line up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Columns {
    /// Box Mode, one per line: labels, then right-aligned numbers, then units.
    Stacked,
    /// Box Mode, grouped by device: each line packed on its own after its label.
    Inline,
    /// Line / Pancake Mode: columns that line up across the rows; in each one the values
    /// start right after the column's labels.
    Grid,
}

/// What a piece of placed text is, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Label,
    Value,
    Unit,
}

/// One piece of text at its place: `x` from the content's left edge, on line `row`.
#[derive(Debug, Clone, PartialEq)]
struct Placed {
    text: String,
    x: f32,
    row: usize,
    part: Part,
    dim: bool,
}

impl Placed {
    fn label(text: &str, x: f32, row: usize) -> Placed {
        Placed { text: text.to_string(), x, row, part: Part::Label, dim: false }
    }
    fn value(it: &Item, x: f32, row: usize) -> Placed {
        Placed { text: it.value.clone(), x, row, part: Part::Value, dim: it.dim }
    }
    fn unit(it: &Item, x: f32, row: usize) -> Placed {
        Placed { text: it.unit.clone(), x, row, part: Part::Unit, dim: it.dim }
    }
}

impl Style {
    pub fn from_config(cfg: &Config, dpi_scale: f32) -> Style {
        let o = &cfg.overlay;
        let s = o.scale * dpi_scale;
        Style {
            px: (o.font_size * s).max(4.0),
            padding: o.padding * s,
            line_spacing: o.line_spacing * s,
            column_gap: o.column_gap * s,
            corner_radius: o.corner_radius * s,
            label: o.label_color,
            value: o.value_color,
            unit: o.unit_color,
            background: o.background,
            opacity: o.opacity,
            shadow: o.text_shadow,
            columns: match (o.display_mode, o.layout) {
                (DisplayMode::Pancake, _) => Columns::Grid,
                (DisplayMode::Box, LayoutMode::Stacked) => Columns::Stacked,
                (DisplayMode::Box, LayoutMode::Grouped) => Columns::Inline,
            },
        }
    }
}

struct Glyph {
    metrics: fontdue::Metrics,
    bitmap: Vec<u8>,
}

/// Text rasterizer with a glyph cache. The cache is keyed by character and pixel size and
/// is cleared whenever the size or font changes, so it stays a few dozen entries.
pub struct Renderer {
    primary: fontdue::Font,
    /// Used for characters the primary font lacks (e.g. "°" in some system fonts).
    fallback: fontdue::Font,
    cache: HashMap<(char, bool), Glyph>,
    cache_px: f32,
    pub font_description: String,
}

impl Renderer {
    /// Load the configured font, falling back to the bundled font (with a reason) on error.
    pub fn new(choice: &FontChoice, weight: FontWeight) -> (Renderer, Option<String>) {
        let fallback = load_font(bundled_font_bytes(FontWeight::Regular)).expect("bundled font parses");
        let bundled = || load_font(bundled_font_bytes(weight)).expect("bundled font parses");
        // (display name, file to load) for non-bundled choices.
        let wanted: Option<(String, Option<&String>)> = match choice {
            FontChoice::Bundled => None,
            FontChoice::System { family, path } => Some((family.clone(), path.as_ref())),
            FontChoice::File { path } => Some((path.clone(), Some(path))),
        };
        let (primary, desc, err) = match wanted {
            None => (bundled(), BUNDLED_FONT_NAME.to_string(), None),
            Some((name, Some(p))) if !p.is_empty() => match load_font_file(p) {
                Ok(f) => (f, name, None),
                Err(e) => (bundled(), BUNDLED_FONT_NAME.to_string(), Some(format!("font '{name}' could not be loaded ({e}); using bundled font"))),
            },
            Some((name, _)) => (bundled(), BUNDLED_FONT_NAME.to_string(), Some(format!("font '{name}' was not found; using bundled font"))),
        };
        (Renderer { primary, fallback, cache: HashMap::new(), cache_px: 0.0, font_description: desc }, err)
    }

    fn face(&self, c: char) -> (&fontdue::Font, bool) {
        if self.primary.lookup_glyph_index(c) != 0 || c == ' ' { (&self.primary, false) } else { (&self.fallback, true) }
    }

    /// Rasterize `c` at `px` into the cache if needed and return its cache key.
    fn ensure_glyph(&mut self, c: char, px: f32) -> (char, bool) {
        if px != self.cache_px {
            self.cache.clear();
            self.cache_px = px;
        }
        let (font, fb) = self.face(c);
        // `font` borrows `self`, so the entry API cannot be used here.
        #[allow(clippy::map_entry)]
        if !self.cache.contains_key(&(c, fb)) {
            let (metrics, bitmap) = font.rasterize(c, px);
            self.cache.insert((c, fb), Glyph { metrics, bitmap });
        }
        (c, fb)
    }

    fn glyph(&mut self, c: char, px: f32) -> &Glyph {
        let key = self.ensure_glyph(c, px);
        &self.cache[&key]
    }

    pub fn text_width(&mut self, s: &str, px: f32) -> f32 {
        let mut w = 0.0;
        let mut prev: Option<char> = None;
        for c in s.chars() {
            if let Some(p) = prev {
                w += self.primary.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            w += self.glyph(c, px).metrics.advance_width;
            prev = Some(c);
        }
        w
    }

    fn line_metrics(&self, px: f32) -> (f32, f32) {
        match self.primary.horizontal_line_metrics(px) {
            Some(m) => (m.ascent, m.ascent - m.descent),
            None => (px * 0.8, px * 1.2),
        }
    }

    /// Where each piece of text goes, and the content's width.
    fn layout(&mut self, rows: &[Row], st: &Style) -> (Vec<Placed>, f32) {
        let px = st.px;
        let unit_gap = px * 0.22;
        let gap = st.column_gap;
        let mut out = Vec::new();
        let mut width = 0.0f32;
        match st.columns {
            Columns::Stacked => {
                let label_w = rows.iter().filter_map(|r| r.label.as_deref()).map(|l| self.text_width(l, px)).fold(0.0f32, f32::max);
                // Only labelled lines share the number column.
                let (mut value_col, mut unit_col) = (0.0f32, 0.0f32);
                for it in rows.iter().filter(|r| r.label.is_some()).filter_map(|r| r.items.first()) {
                    value_col = value_col.max(self.text_width(&it.value, px));
                    unit_col = unit_col.max(self.unit_width(&it.unit, px, unit_gap));
                }
                for (ri, r) in rows.iter().enumerate() {
                    let Some(it) = r.items.first() else { continue };
                    match &r.label {
                        Some(l) => {
                            out.push(Placed::label(l, 0.0, ri));
                            let x = label_w + gap;
                            let vw = self.text_width(&it.value, px);
                            out.push(Placed::value(it, x + value_col - vw, ri)); // right-aligned so digits line up
                            if !it.unit.is_empty() {
                                out.push(Placed::unit(it, x + value_col + self.unit_offset(&it.unit, unit_gap), ri));
                            }
                            width = width.max(x + value_col + unit_col);
                        }
                        // Without its label, the value starts at the left edge.
                        None => width = width.max(self.place_item(&mut out, it, 0.0, ri, px, unit_gap)),
                    }
                }
            }
            Columns::Inline => {
                let label_w = rows.iter().filter_map(|r| r.label.as_deref()).map(|l| self.text_width(l, px)).fold(0.0f32, f32::max);
                for (ri, r) in rows.iter().enumerate() {
                    let mut x = 0.0;
                    if let Some(l) = &r.label {
                        out.push(Placed::label(l, 0.0, ri));
                        x = label_w + gap;
                    }
                    for (i, it) in r.items.iter().enumerate() {
                        if i > 0 {
                            x += gap;
                        }
                        if let Some(p) = &it.prefix {
                            out.push(Placed::label(p, x, ri));
                            x += self.text_width(p, px) + unit_gap * 2.0;
                        }
                        x = self.place_item(&mut out, it, x, ri, px, unit_gap);
                    }
                    width = width.max(x);
                }
            }
            Columns::Grid => {
                let ncols = rows.iter().map(|r| r.items.len()).max().unwrap_or(0);
                let mut label_w = vec![0.0f32; ncols];
                for r in rows {
                    for (c, it) in r.items.iter().enumerate() {
                        if let Some(p) = &it.prefix {
                            label_w[c] = label_w[c].max(self.text_width(p, px));
                        }
                    }
                }
                let mut col_w = vec![0.0f32; ncols];
                for r in rows {
                    for (c, it) in r.items.iter().enumerate() {
                        let lead = if it.prefix.is_some() { label_w[c] + unit_gap * 2.0 } else { 0.0 };
                        let w = lead + self.text_width(&it.value, px) + self.unit_width(&it.unit, px, unit_gap);
                        col_w[c] = col_w[c].max(w);
                    }
                }
                let mut col_x = Vec::with_capacity(ncols);
                let mut x = 0.0;
                for w in &col_w {
                    col_x.push(x);
                    x += w + gap;
                }
                for (ri, r) in rows.iter().enumerate() {
                    for (c, it) in r.items.iter().enumerate() {
                        let mut x = col_x[c];
                        if let Some(p) = &it.prefix {
                            out.push(Placed::label(p, x, ri));
                            x += label_w[c] + unit_gap * 2.0;
                        }
                        // Without its label, the value starts at the column's left edge.
                        width = width.max(self.place_item(&mut out, it, x, ri, px, unit_gap));
                    }
                }
            }
        }
        (out, width)
    }

    /// Place a value and its unit from `x`; returns where they end.
    fn place_item(&mut self, out: &mut Vec<Placed>, it: &Item, x: f32, row: usize, px: f32, unit_gap: f32) -> f32 {
        out.push(Placed::value(it, x, row));
        let mut end = x + self.text_width(&it.value, px);
        if !it.unit.is_empty() {
            end += self.unit_offset(&it.unit, unit_gap);
            out.push(Placed::unit(it, end, row));
            end += self.text_width(&it.unit, px);
        }
        end
    }

    /// Lay out and rasterize the rows. Returns an empty 0x0 pixmap when there is nothing to show.
    pub fn render(&mut self, rows: &[Row], st: &Style) -> Pixmap {
        if rows.is_empty() {
            return Pixmap::new(0, 0);
        }
        let px = st.px;
        let (ascent, line_h) = self.line_metrics(px);
        let (placed, content_w) = self.layout(rows, st);
        let shadow_pad = if st.shadow { (px / 14.0).ceil().max(1.0) } else { 0.0 };
        let width = (content_w + st.padding * 2.0 + shadow_pad).ceil().max(1.0) as u32;
        let height = (rows.len() as f32 * line_h + (rows.len() as f32 - 1.0) * st.line_spacing + st.padding * 2.0 + shadow_pad).ceil().max(1.0) as u32;

        let mut pm = Pixmap::new(width, height);
        fill_rounded_rect(&mut pm, st.background, st.corner_radius);
        for p in &placed {
            let baseline = st.padding + p.row as f32 * (line_h + st.line_spacing) + ascent;
            let color = match p.part {
                Part::Label => st.label,
                Part::Value => dimmed(st.value, p.dim),
                Part::Unit => dimmed(st.unit, p.dim),
            };
            self.draw_text(&mut pm, &p.text, st.padding + p.x, baseline, px, color, st.shadow);
        }

        if st.opacity < 1.0 {
            let k = (st.opacity.clamp(0.0, 1.0) * 256.0) as u32;
            for b in pm.data.iter_mut() {
                *b = ((*b as u32 * k) >> 8) as u8;
            }
        }
        pm
    }

    /// "%" hugs the number; other units get a small gap.
    fn unit_offset(&self, unit: &str, gap: f32) -> f32 {
        if unit == "%" { gap * 0.3 } else { gap }
    }

    fn unit_width(&mut self, unit: &str, px: f32, gap: f32) -> f32 {
        if unit.is_empty() { 0.0 } else { self.unit_offset(unit, gap) + self.text_width(unit, px) }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(&mut self, pm: &mut Pixmap, s: &str, x: f32, baseline: f32, px: f32, color: Rgba, shadow: bool) {
        let off = (px / 14.0).ceil().max(1.0) as i32;
        let mut pen = x;
        let mut prev: Option<char> = None;
        let shadow_color = Rgba(0, 0, 0, (color.3 as u32 * 200 / 255) as u8);
        for c in s.chars() {
            if let Some(p) = prev {
                pen += self.primary.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            let key = self.ensure_glyph(c, px);
            let g = &self.cache[&key];
            let m = &g.metrics;
            let gx = pen.round() as i32 + m.xmin;
            let gy = baseline.round() as i32 - m.height as i32 - m.ymin;
            if shadow {
                blit_coverage(pm, &g.bitmap, m.width, m.height, gx + off, gy + off, shadow_color);
            }
            blit_coverage(pm, &g.bitmap, m.width, m.height, gx, gy, color);
            pen += m.advance_width;
            prev = Some(c);
        }
    }
}

fn dimmed(c: Rgba, dim: bool) -> Rgba {
    if dim { Rgba(c.0, c.1, c.2, (c.3 as u32 * 45 / 100) as u8) } else { c }
}

fn load_font(bytes: &[u8]) -> Result<fontdue::Font, String> {
    fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).map_err(|e| e.to_string())
}

/// Load a font file; a `#N` suffix selects face N of a TrueType collection.
pub fn load_font_file(spec: &str) -> Result<fontdue::Font, String> {
    let (path, index) = match spec.rsplit_once('#') {
        Some((p, i)) if i.chars().all(|c| c.is_ascii_digit()) && !i.is_empty() => (p, i.parse().unwrap_or(0)),
        _ => (spec, 0),
    };
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > 64 * 1024 * 1024 {
        return Err("file is larger than 64 MB".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let settings = fontdue::FontSettings { collection_index: index, ..Default::default() };
    fontdue::Font::from_bytes(bytes, settings).map_err(|e| e.to_string())
}

/// Composite a coverage mask in `color` over the pixmap (premultiplied "over").
fn blit_coverage(pm: &mut Pixmap, cov: &[u8], w: usize, h: usize, x0: i32, y0: i32, color: Rgba) {
    let (pw, ph) = (pm.width as i32, pm.height as i32);
    for y in 0..h as i32 {
        let py = y0 + y;
        if py < 0 || py >= ph {
            continue;
        }
        for x in 0..w as i32 {
            let px = x0 + x;
            if px < 0 || px >= pw {
                continue;
            }
            let c = cov[(y as usize) * w + x as usize] as u32;
            if c == 0 {
                continue;
            }
            let a = c * color.3 as u32 / 255; // source alpha
            let i = ((py * pw + px) * 4) as usize;
            let d = &mut pm.data[i..i + 4];
            let inv = 255 - a;
            d[0] = ((color.2 as u32 * a + d[0] as u32 * inv) / 255) as u8;
            d[1] = ((color.1 as u32 * a + d[1] as u32 * inv) / 255) as u8;
            d[2] = ((color.0 as u32 * a + d[2] as u32 * inv) / 255) as u8;
            d[3] = (a + d[3] as u32 * inv / 255) as u8;
        }
    }
}

/// Fill the whole pixmap with an anti-aliased rounded rectangle.
fn fill_rounded_rect(pm: &mut Pixmap, color: Rgba, radius: f32) {
    if color.3 == 0 {
        return;
    }
    let (w, h) = (pm.width as f32, pm.height as f32);
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    for y in 0..pm.height {
        for x in 0..pm.width {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(r, w - r);
            let cy = fy.clamp(r, h - r);
            let dist = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let cov = (r - dist + 0.5).clamp(0.0, 1.0);
            let cov = if r == 0.0 { 1.0 } else { cov };
            let a = (color.3 as f32 * cov) as u32;
            let i = ((y * pm.width + x) * 4) as usize;
            pm.data[i] = (color.2 as u32 * a / 255) as u8;
            pm.data[i + 1] = (color.1 as u32 * a / 255) as u8;
            pm.data[i + 2] = (color.0 as u32 * a / 255) as u8;
            pm.data[i + 3] = a as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Anchor, MetricEntry};
    use crate::metrics::{DeviceInfo, Freshness, Reading, Vendor};

    fn snap() -> Snapshot {
        let mut s = Snapshot::default();
        s.devices.push(DeviceInfo {
            id: "gpu:0".into(),
            kind: DeviceKind::Gpu,
            name: "GPU".into(),
            vendor: Vendor::Nvidia,
            integrated: false,
            provider: "t".into(),
        });
        s.devices.push(DeviceInfo {
            id: "cpu:0".into(),
            kind: DeviceKind::Cpu,
            name: "CPU".into(),
            vendor: Vendor::Intel,
            integrated: false,
            provider: "t".into(),
        });
        let mut add = |dev: &str, m: MetricId, v: f64| {
            s.readings.push(Reading {
                metric: m,
                device: dev.into(),
                value: Some(v),
                unit: m.unit(),
                freshness: Freshness::Fresh,
                age_ms: Some(0),
                reason: None,
                source: "t".into(),
            })
        };
        add("gpu:0", MetricId::GpuUsage, 97.0);
        add("gpu:0", MetricId::GpuTemp, 66.0);
        add("cpu:0", MetricId::CpuUsage, 12.0);
        add(DEVICE_GAME, MetricId::Fps, 144.0);
        add(DEVICE_GAME, MetricId::FrameTime, 6.94);
        s
    }

    fn only(ids: &[MetricId]) -> Config {
        Config { metrics: ids.iter().map(|&m| MetricEntry::new(m, true)).collect(), ..Config::default() }
    }

    #[test]
    fn rows_follow_order_and_selection() {
        let c = only(&[MetricId::CpuUsage, MetricId::GpuTemp, MetricId::Fps, MetricId::FrameTime]);
        let rows = build_rows(&snap(), &c);
        let vals: Vec<_> = rows.iter().map(|r| (r.label.clone().unwrap(), r.items[0].value.clone())).collect();
        assert_eq!(
            vals,
            vec![
                ("CPU".to_string(), "12".to_string()),
                ("GPU Temp".to_string(), "66".to_string()),
                ("FPS".to_string(), "144".to_string()),
                ("Frame Time".to_string(), "6.9".to_string()),
            ]
        );
    }

    #[test]
    fn missing_metric_renders_na_not_zero() {
        let c = only(&[MetricId::GpuPower]);
        let rows = build_rows(&snap(), &c);
        assert_eq!(rows[0].items[0].value, "N/A");
        assert!(rows[0].items[0].dim);
    }

    #[test]
    fn grouped_layout_merges_consecutive_group_members() {
        let mut c = only(&[MetricId::GpuUsage, MetricId::GpuTemp, MetricId::CpuUsage]);
        c.overlay.layout = LayoutMode::Grouped;
        let rows = build_rows(&snap(), &c);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label.as_deref(), Some("GPU"));
        assert_eq!(rows[0].items.len(), 2);
        // Custom group moves a metric into its own row.
        c.metrics[1].group = Some("HOT".into());
        assert_eq!(build_rows(&snap(), &c).len(), 3);
    }

    fn pancake(ids: &[MetricId], rows: u32) -> Config {
        let mut c = only(ids);
        c.overlay.display_mode = DisplayMode::Pancake;
        c.overlay.pancake_rows = rows;
        c
    }

    const EIGHT: [MetricId; 8] = [
        MetricId::Fps,
        MetricId::FrameTime,
        MetricId::GpuUsage,
        MetricId::GpuTemp,
        MetricId::VramUsed,
        MetricId::CpuUsage,
        MetricId::CpuTemp,
        MetricId::RamUsed,
    ];

    #[test]
    fn one_row_pancake_is_a_single_line() {
        let rows = build_rows(&snap(), &pancake(&[MetricId::Fps, MetricId::GpuUsage], 1));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, None);
        assert_eq!(rows[0].items[0].prefix.as_deref(), Some("FPS"));
        assert_eq!(rows[0].items[1].prefix.as_deref(), Some("GPU"));
    }

    #[test]
    fn pancake_fills_rows_top_first_and_evenly() {
        let counts = |ids: &[MetricId], n: u32| build_rows(&snap(), &pancake(ids, n)).iter().map(|r| r.items.len()).collect::<Vec<_>>();
        assert_eq!(counts(&EIGHT, 1), vec![8]);
        assert_eq!(counts(&EIGHT, 2), vec![4, 4]);
        assert_eq!(counts(&EIGHT, 3), vec![3, 3, 2]);
        assert_eq!(counts(&EIGHT[..7], 2), vec![4, 3]);
        assert_eq!(counts(&EIGHT[..7], 3), vec![3, 2, 2]);
        // More rows than statistics: one statistic per row.
        assert_eq!(counts(&EIGHT, 20), vec![1; 8]);
        assert!(build_rows(&snap(), &pancake(&[], 3)).is_empty());
        // Row by row, in the configured order.
        let rows = build_rows(&snap(), &pancake(&EIGHT, 2));
        let first: Vec<_> = rows[1].items.iter().map(|i| i.prefix.clone().unwrap()).collect();
        assert_eq!(first, ["VRAM", "CPU", "CPU Temp", "RAM"]);
    }

    #[test]
    fn labels_can_be_hidden_one_by_one_or_all_at_once() {
        let ids = [MetricId::Fps, MetricId::CpuUsage, MetricId::CpuTemp];
        let mut c = only(&ids);
        c.metrics[1].show_label = false;
        let labels = |c: &Config| build_rows(&snap(), c).iter().map(|r| r.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&c), [Some("FPS".to_string()), None, Some("CPU Temp".to_string())]);
        // The value is still there.
        assert_eq!(build_rows(&snap(), &c)[1].items[0].value, "12");
        c.overlay.show_labels = false;
        assert_eq!(labels(&c), [None, None, None]);
        // Switching the main switch back on restores each statistic's own choice.
        c.overlay.show_labels = true;
        assert_eq!(labels(&c)[1], None);

        let mut p = pancake(&ids, 1);
        p.metrics[0].show_label = false;
        let prefixes: Vec<_> = build_rows(&snap(), &p)[0].items.iter().map(|i| i.prefix.clone()).collect();
        assert_eq!(prefixes, [None, Some("CPU".to_string()), Some("CPU Temp".to_string())]);
        p.overlay.show_labels = false;
        assert!(build_rows(&snap(), &p)[0].items.iter().all(|i| i.prefix.is_none()));
    }

    #[test]
    fn grouped_layout_follows_only_the_main_label_switch() {
        let mut c = only(&[MetricId::GpuUsage, MetricId::GpuTemp, MetricId::CpuUsage]);
        c.overlay.layout = LayoutMode::Grouped;
        c.metrics[0].show_label = false;
        c.metrics[1].show_label = false;
        let rows = build_rows(&snap(), &c);
        assert_eq!(rows[0].label.as_deref(), Some("GPU"), "per-statistic switches do not apply");
        c.overlay.show_labels = false;
        let rows = build_rows(&snap(), &c);
        assert_eq!(rows.len(), 2, "still grouped");
        assert!(rows.iter().all(|r| r.label.is_none()));
    }

    /// Positions are sums of glyph widths, so compare them within a thousandth of a pixel.
    macro_rules! near {
        ($a:expr, $b:expr) => {{
            let (a, b): (f32, f32) = ($a, $b);
            assert!((a - b).abs() < 1e-3, "{} = {a}, expected {b}", stringify!($a));
        }};
    }

    fn at(placed: &[Placed], text: &str, row: usize) -> f32 {
        placed.iter().find(|p| p.text == text && p.row == row).unwrap_or_else(|| panic!("{text} on row {row}: {placed:?}")).x
    }

    #[test]
    fn one_row_pancake_lays_out_like_the_old_single_line() {
        let c = pancake(&[MetricId::Fps, MetricId::GpuTemp], 1);
        let st = Style::from_config(&c, 1.0);
        let (mut r, _) = Renderer::new(&FontChoice::Bundled, FontWeight::SemiBold);
        let (placed, width) = r.layout(&build_rows(&snap(), &c), &st);
        // The old single line: label, a gap of two unit gaps, value, unit, column gap, next.
        let (px, ug) = (st.px, st.px * 0.22);
        let fps_value = r.text_width("FPS", px) + ug * 2.0;
        near!(at(&placed, "FPS", 0), 0.0);
        near!(at(&placed, "144", 0), fps_value);
        let next = fps_value + r.text_width("144", px) + st.column_gap;
        near!(at(&placed, "GPU Temp", 0), next);
        let temp_value = next + r.text_width("GPU Temp", px) + ug * 2.0;
        near!(at(&placed, "66", 0), temp_value);
        let unit = placed.iter().find(|p| p.part == Part::Unit).unwrap();
        near!(unit.x, temp_value + r.text_width("66", px) + ug);
        near!(width, unit.x + r.text_width(&unit.text, px));
    }

    #[test]
    fn pancake_columns_line_up_and_values_follow_their_labels() {
        // Row 0: FPS, GPU Temp. Row 1: CPU (label off), Frame Time.
        let mut c = pancake(&[MetricId::Fps, MetricId::GpuTemp, MetricId::CpuUsage, MetricId::FrameTime], 2);
        c.metrics[2].show_label = false;
        let st = Style::from_config(&c, 1.0);
        let (mut r, _) = Renderer::new(&FontChoice::Bundled, FontWeight::SemiBold);
        let (placed, _) = r.layout(&build_rows(&snap(), &c), &st);
        let (px, ug) = (st.px, st.px * 0.22);
        // Column 0: FPS 144 above the unlabelled CPU value, which starts at the column's edge.
        near!(at(&placed, "FPS", 0), 0.0);
        near!(at(&placed, "144", 0), r.text_width("FPS", px) + ug * 2.0); // right after the column's label
        near!(at(&placed, "12", 1), 0.0); // an unlabelled value moves left
        // Column 1 starts at the same place on both rows, after column 0's widest cell.
        let col0 = (r.text_width("FPS", px) + ug * 2.0 + r.text_width("144", px)).max(r.text_width("12", px) + ug * 0.3 + r.text_width("%", px));
        let col1 = col0 + st.column_gap;
        near!(at(&placed, "GPU Temp", 0), col1);
        near!(at(&placed, "Frame Time", 1), col1);
        // Values in column 1 start together, after its widest label, left-aligned.
        let v1 = col1 + r.text_width("GPU Temp", px).max(r.text_width("Frame Time", px)) + ug * 2.0;
        near!(at(&placed, "66", 0), v1);
        near!(at(&placed, "6.9", 1), v1);
    }

    #[test]
    fn box_value_without_label_moves_to_the_left_edge() {
        let mut c = only(&[MetricId::Fps, MetricId::CpuUsage, MetricId::GpuTemp]);
        c.metrics[1].show_label = false;
        let st = Style::from_config(&c, 1.0);
        assert_eq!(st.columns, Columns::Stacked);
        let (mut r, _) = Renderer::new(&FontChoice::Bundled, FontWeight::SemiBold);
        let (placed, width) = r.layout(&build_rows(&snap(), &c), &st);
        let px = st.px;
        near!(at(&placed, "12", 1), 0.0);
        // The labelled ones still right-align their numbers in one column.
        let label_w = r.text_width("FPS", px).max(r.text_width("GPU Temp", px));
        let num_end = label_w + st.column_gap + r.text_width("144", px).max(r.text_width("66", px));
        near!(at(&placed, "144", 0) + r.text_width("144", px), num_end);
        near!(at(&placed, "66", 2) + r.text_width("66", px), num_end);
        // All labels off: every value at the left edge, and the box narrows.
        c.overlay.show_labels = false;
        let (placed, narrow) = r.layout(&build_rows(&snap(), &c), &st);
        assert!(placed.iter().filter(|p| p.part == Part::Value).all(|p| p.x == 0.0));
        assert!(placed.iter().all(|p| p.part != Part::Label));
        assert!(narrow < width);
    }

    #[test]
    fn style_follows_the_switch() {
        let mut c = only(&[MetricId::Fps]);
        assert_eq!(Style::from_config(&c, 1.0).columns, Columns::Stacked);
        c.overlay.layout = LayoutMode::Grouped;
        assert_eq!(Style::from_config(&c, 1.0).columns, Columns::Inline);
        c.overlay.display_mode = DisplayMode::Pancake;
        assert_eq!(Style::from_config(&c, 1.0).columns, Columns::Grid, "Box Mode's layout does not apply");
    }

    #[test]
    fn pancake_renders_wider_than_tall() {
        let (mut r, _) = Renderer::new(&FontChoice::Bundled, FontWeight::SemiBold);
        let render = |r: &mut Renderer, c: &Config| r.render(&build_rows(&snap(), c), &Style::from_config(c, 1.0));
        let boxed = render(&mut r, &only(&EIGHT));
        let one = render(&mut r, &pancake(&EIGHT, 1));
        let two = render(&mut r, &pancake(&EIGHT, 2));
        assert!(one.width > boxed.width && one.height < boxed.height, "box {boxed:?}, pancake {one:?}");
        assert!(two.height > one.height && two.width < one.width, "1 row {one:?}, 2 rows {two:?}");
    }

    #[test]
    fn custom_and_compact_labels() {
        let mut c = only(&[MetricId::GpuTemp]);
        c.overlay.compact_labels = true;
        assert_eq!(build_rows(&snap(), &c)[0].label.as_deref(), Some("GPU"));
        c.metrics[0].label = Some("Hot".into());
        assert_eq!(build_rows(&snap(), &c)[0].label.as_deref(), Some("Hot"));
    }

    #[test]
    fn renders_nonempty_pixmap_with_transparent_corners() {
        let mut c = only(&[MetricId::Fps, MetricId::GpuUsage]);
        c.overlay.corner_radius = 6.0;
        c.overlay.anchor = Anchor::TopLeft;
        let (mut r, err) = Renderer::new(&FontChoice::Bundled, FontWeight::SemiBold);
        assert!(err.is_none());
        let pm = r.render(&build_rows(&snap(), &c), &Style::from_config(&c, 1.0));
        assert!(pm.width > 40 && pm.height > 20, "{pm:?}");
        assert_eq!(pm.pixel(0, 0)[3], 0, "rounded corner should be transparent");
        // Some pixel must be bright text.
        assert!(pm.data.as_chunks::<4>().0.iter().any(|p| p[0] > 200 && p[3] > 200));
        // Premultiplied invariant: colour channels never exceed alpha.
        assert!(pm.data.as_chunks::<4>().0.iter().all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
    }

    #[test]
    fn scale_grows_output_and_opacity_reduces_alpha() {
        let c = only(&[MetricId::Fps]);
        let (mut r, _) = Renderer::new(&FontChoice::Bundled, FontWeight::Regular);
        let rows = build_rows(&snap(), &c);
        let a = r.render(&rows, &Style::from_config(&c, 1.0));
        let b = r.render(&rows, &Style::from_config(&c, 2.0));
        assert!(b.width > a.width * 3 / 2 && b.height > a.height * 3 / 2);
        let mut half = Style::from_config(&c, 1.0);
        half.opacity = 0.5;
        let h = r.render(&rows, &half);
        let max_a = |p: &Pixmap| p.data.as_chunks::<4>().0.iter().map(|x| x[3]).max().unwrap();
        assert!(max_a(&h) <= max_a(&a) / 2 + 2);
    }

    #[test]
    fn no_metrics_gives_empty_pixmap() {
        let c = only(&[]);
        let (mut r, _) = Renderer::new(&FontChoice::Bundled, FontWeight::Regular);
        let pm = r.render(&build_rows(&snap(), &c), &Style::from_config(&c, 1.0));
        assert_eq!((pm.width, pm.height), (0, 0));
    }

    #[test]
    fn bad_font_file_falls_back_with_reason() {
        let (r, err) = Renderer::new(&FontChoice::File { path: "/definitely/not/here.ttf".into() }, FontWeight::Regular);
        assert!(err.unwrap().contains("could not be loaded"));
        assert_eq!(r.font_description, BUNDLED_FONT_NAME);
    }

    #[test]
    fn unpremultiply_round_trip() {
        let pm = Pixmap { width: 1, height: 1, data: vec![64, 32, 16, 128] };
        assert_eq!(pm.to_rgba_unpremultiplied(), vec![32, 64, 128, 128]);
    }
}
