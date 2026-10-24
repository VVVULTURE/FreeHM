//! Persistent configuration: schema, defaults, validation, migration and crash-safe saving.
//!
//! The file is JSON (human-editable, and serde_json is already needed for IPC). Unknown
//! fields are ignored and missing fields take defaults, so older and newer files both load.
//! After loading, [`Config::sanitize`] clamps or replaces every out-of-range value.

use crate::hotkey::Hotkey;
use crate::metrics::MetricId;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_REFRESH_MS: u32 = 200;
pub const MIN_REFRESH_MS: u32 = 50;
pub const MAX_REFRESH_MS: u32 = 5000;
pub const DEFAULT_DETECT_MS: u32 = 2000;
pub const DEFAULT_HOTKEY: &str = "Ctrl+Shift+O";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}{:02X}", self.0, self.1, self.2, self.3)
    }
    pub fn from_hex(s: &str) -> Option<Rgba> {
        let s = s.trim().trim_start_matches('#');
        let p = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
        match s.len() {
            6 => Some(Rgba(p(0)?, p(2)?, p(4)?, 255)),
            8 => Some(Rgba(p(0)?, p(2)?, p(4)?, p(6)?)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TemperatureUnit {
    Celsius,
    Fahrenheit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Hardware polling and overlay refresh interval.
    pub refresh_interval_ms: u32,
    pub temperature_unit: TemperatureUnit,
    pub start_with_system: bool,
    /// Show clocks in GHz instead of MHz.
    pub clock_in_ghz: bool,
}

impl Default for General {
    fn default() -> Self {
        General { refresh_interval_ms: DEFAULT_REFRESH_MS, temperature_unit: TemperatureUnit::Celsius, start_with_system: true, clock_in_ghz: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontWeight {
    Regular,
    Medium,
    SemiBold,
    Bold,
}

/// Which font the overlay uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FontChoice {
    /// The bundled Exo 2 (SIL OFL 1.1), a legal look-alike for NVIDIA Sans.
    Bundled,
    /// An installed system font, by family name. `path` caches the resolved file.
    System { family: String, path: Option<String> },
    /// Any font file chosen by the user (never redistributed by FreeHM).
    File { path: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutMode {
    /// One metric per line: label column + right-aligned value column.
    Stacked,
    /// One line per group ("GPU  45 %  62 °C  1950 MHz").
    Grouped,
    /// Everything on a single horizontal line.
    SingleLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
    /// `offset_x`/`offset_y` are the top-left position in logical pixels relative to the
    /// monitor's top-left corner.
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MonitorChoice {
    Primary,
    /// Platform monitor name (e.g. `\\.\DISPLAY2`, `DP-1`); falls back to primary if absent.
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    pub font: FontChoice,
    /// Logical pixels at 100 % display scaling.
    pub font_size: f32,
    pub font_weight: FontWeight,
    pub label_color: Rgba,
    pub value_color: Rgba,
    pub unit_color: Rgba,
    pub background: Rgba,
    /// Multiplies the whole overlay, 0.1–1.0.
    pub opacity: f32,
    pub padding: f32,
    pub line_spacing: f32,
    pub column_gap: f32,
    pub corner_radius: f32,
    pub text_shadow: bool,
    pub layout: LayoutMode,
    pub compact_labels: bool,
    pub show_units: bool,
    /// Extra scale on top of the monitor's DPI scale, 0.5–3.0.
    pub scale: f32,
    pub anchor: Anchor,
    pub monitor: MonitorChoice,
    /// Distance from the anchored screen edges, logical pixels.
    pub margin_x: f32,
    pub margin_y: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    /// Show on the desktop when no game is active.
    pub show_on_desktop: bool,
    /// Windows: ask DWM to exclude the overlay from screenshots/recordings.
    pub hide_from_capture: bool,
    /// Windows draws nothing over a game in exclusive fullscreen; while a game holds its
    /// monitor that way, show the overlay on another monitor (when there is one).
    pub avoid_exclusive_fullscreen: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        OverlayConfig {
            font: FontChoice::Bundled,
            font_size: 15.0,
            font_weight: FontWeight::SemiBold,
            label_color: Rgba(118, 185, 0, 255), // NVIDIA-like green labels
            value_color: Rgba(255, 255, 255, 255),
            unit_color: Rgba(200, 200, 200, 255),
            background: Rgba(0, 0, 0, 110),
            opacity: 1.0,
            padding: 8.0,
            line_spacing: 2.0,
            column_gap: 14.0,
            corner_radius: 4.0,
            text_shadow: true,
            layout: LayoutMode::Stacked,
            compact_labels: false,
            show_units: true,
            scale: 1.0,
            anchor: Anchor::TopRight,
            monitor: MonitorChoice::Primary,
            margin_x: 16.0,
            margin_y: 16.0,
            offset_x: 100.0,
            offset_y: 100.0,
            show_on_desktop: false,
            hide_from_capture: false,
            avoid_exclusive_fullscreen: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricEntry {
    pub id: MetricId,
    pub enabled: bool,
    /// Custom label; `None` uses the default (or compact) label.
    #[serde(default)]
    pub label: Option<String>,
    /// Custom group for the grouped layout; `None` uses the metric's default group.
    #[serde(default)]
    pub group: Option<String>,
    /// For RAM/VRAM used: render as "used / total".
    #[serde(default)]
    pub show_total: bool,
}

impl MetricEntry {
    pub fn new(id: MetricId, enabled: bool) -> Self {
        let show_total = matches!(id, MetricId::RamUsed | MetricId::VramUsed);
        MetricEntry { id, enabled, label: None, group: None, show_total }
    }
}

pub fn default_metrics() -> Vec<MetricEntry> {
    use MetricId::*;
    let on = [Fps, FrameTime, GpuUsage, GpuTemp, VramUsed, CpuUsage, CpuTemp, RamUsed];
    MetricId::ALL.iter().map(|&m| MetricEntry::new(m, on.contains(&m))).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HardwareConfig {
    /// Selected CPU device id; `None` = automatic.
    pub cpu_device: Option<String>,
    /// Selected GPU device id; `None` = automatic (first discrete GPU).
    pub gpu_device: Option<String>,
    /// Windows: use the privileged helper service (CPU temperature, FPS) when installed.
    pub use_helper: HelperUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HelperUse {
    #[default]
    Enabled,
    /// Never contact the helper (e.g. while playing a game whose anti-cheat dislikes drivers).
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShowFor {
    AllDetectedGames,
    SelectedGamesOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectionConfig {
    pub auto_detect: bool,
    pub show_for: ShowFor,
    /// Executable names (`game.exe`) or full paths always treated as games.
    pub manual_games: Vec<String>,
    /// Executable names or full paths never treated as games.
    pub excluded: Vec<String>,
    /// Treat an unknown foreground application that covers a whole monitor as a game.
    pub detect_fullscreen_apps: bool,
    pub interval_ms: u32,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        DetectionConfig {
            auto_detect: true,
            show_for: ShowFor::AllDetectedGames,
            manual_games: Vec::new(),
            excluded: Vec::new(),
            detect_fullscreen_apps: true,
            interval_ms: DEFAULT_DETECT_MS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub toggle_overlay: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        HotkeyConfig { toggle_overlay: DEFAULT_HOTKEY.to_string() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PermissionState {
    /// The user has not run the hardware-access setup yet.
    #[default]
    NotConfigured,
    Configured,
    /// The user declined; FreeHM runs with reduced metrics and does not ask again on its own.
    Declined,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PermissionConfig {
    pub state: PermissionState,
    /// First-run onboarding (settings window shown once) has happened.
    pub first_run_done: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub general: General,
    pub overlay: OverlayConfig,
    pub metrics: Vec<MetricEntry>,
    pub hardware: HardwareConfig,
    pub detection: DetectionConfig,
    pub hotkeys: HotkeyConfig,
    pub permissions: PermissionConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            schema_version: SCHEMA_VERSION,
            general: General::default(),
            overlay: OverlayConfig::default(),
            metrics: default_metrics(),
            hardware: HardwareConfig::default(),
            detection: DetectionConfig::default(),
            hotkeys: HotkeyConfig::default(),
            permissions: PermissionConfig::default(),
        }
    }
}

/// Result of loading the configuration file.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    /// Human-readable notes about repairs made while loading (logged, shown in diagnostics).
    pub notes: Vec<String>,
}

impl Config {
    /// Enabled metrics in display order.
    pub fn enabled_metrics(&self) -> impl Iterator<Item = &MetricEntry> {
        self.metrics.iter().filter(|m| m.enabled)
    }

    pub fn is_enabled(&self, id: MetricId) -> bool {
        self.metrics.iter().any(|m| m.id == id && m.enabled)
    }

    /// Clamp/repair every field so the rest of the program can trust the values.
    /// Returns a description of each change made.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        let g = &mut self.general;
        if !(MIN_REFRESH_MS..=MAX_REFRESH_MS).contains(&g.refresh_interval_ms) {
            let v = g.refresh_interval_ms.clamp(MIN_REFRESH_MS, MAX_REFRESH_MS);
            notes.push(format!("refresh interval {} ms out of range, using {} ms", g.refresh_interval_ms, v));
            g.refresh_interval_ms = v;
        }
        let o = &mut self.overlay;
        let mut clampf = |name: &str, v: &mut f32, lo: f32, hi: f32, def: f32| {
            if !v.is_finite() {
                notes.push(format!("{name} invalid, reset to {def}"));
                *v = def;
            } else if *v < lo || *v > hi {
                let c = v.clamp(lo, hi);
                notes.push(format!("{name} {v} out of range, clamped to {c}"));
                *v = c;
            }
        };
        let d = OverlayConfig::default();
        clampf("font size", &mut o.font_size, 6.0, 96.0, d.font_size);
        clampf("opacity", &mut o.opacity, 0.1, 1.0, d.opacity);
        clampf("padding", &mut o.padding, 0.0, 64.0, d.padding);
        clampf("line spacing", &mut o.line_spacing, 0.0, 48.0, d.line_spacing);
        clampf("column gap", &mut o.column_gap, 0.0, 128.0, d.column_gap);
        clampf("corner radius", &mut o.corner_radius, 0.0, 32.0, d.corner_radius);
        clampf("scale", &mut o.scale, 0.5, 3.0, d.scale);
        clampf("margin x", &mut o.margin_x, 0.0, 4000.0, d.margin_x);
        clampf("margin y", &mut o.margin_y, 0.0, 4000.0, d.margin_y);
        clampf("offset x", &mut o.offset_x, -16000.0, 16000.0, d.offset_x);
        clampf("offset y", &mut o.offset_y, -16000.0, 16000.0, d.offset_y);

        // Metrics: drop duplicates and unknowns (serde already rejected unknown ids), then
        // append any metric missing from the list (e.g. added in a newer version) disabled.
        let mut seen = Vec::new();
        self.metrics.retain(|m| {
            if seen.contains(&m.id) {
                false
            } else {
                seen.push(m.id);
                true
            }
        });
        for &m in MetricId::ALL.iter() {
            if !seen.contains(&m) {
                notes.push(format!("metric {m:?} missing from configuration, added (disabled)"));
                self.metrics.push(MetricEntry::new(m, false));
            }
        }
        for m in &mut self.metrics {
            for s in [&mut m.label, &mut m.group] {
                if let Some(t) = s {
                    let trimmed: String = t.trim().chars().take(24).collect();
                    *s = if trimmed.is_empty() { None } else { Some(trimmed) };
                }
            }
        }

        let det = &mut self.detection;
        det.interval_ms = det.interval_ms.clamp(500, 30_000);
        for list in [&mut det.manual_games, &mut det.excluded] {
            list.iter_mut().for_each(|s| *s = s.trim().to_string());
            list.retain(|s| !s.is_empty());
            list.sort_by_key(|s| s.to_lowercase());
            list.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        }

        if Hotkey::parse(&self.hotkeys.toggle_overlay).is_err() {
            notes.push(format!("hotkey '{}' is invalid, reset to {DEFAULT_HOTKEY}", self.hotkeys.toggle_overlay));
            self.hotkeys.toggle_overlay = DEFAULT_HOTKEY.to_string();
        }
        if let FontChoice::System { family, .. } = &self.overlay.font
            && family.trim().is_empty()
        {
            self.overlay.font = FontChoice::Bundled;
        }
        if let FontChoice::File { path } = &self.overlay.font
            && path.trim().is_empty()
        {
            self.overlay.font = FontChoice::Bundled;
        }
        notes
    }

    /// Validate a configuration submitted by the settings UI. Unlike `sanitize`, this
    /// rejects instead of repairing, so the user sees what was wrong.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        let r = self.general.refresh_interval_ms;
        if !(MIN_REFRESH_MS..=MAX_REFRESH_MS).contains(&r) {
            errs.push(format!("Update interval must be between {MIN_REFRESH_MS} and {MAX_REFRESH_MS} ms (got {r} ms)."));
        }
        if let Err(e) = Hotkey::parse(&self.hotkeys.toggle_overlay) {
            errs.push(format!("Shortcut '{}': {e}", self.hotkeys.toggle_overlay));
        }
        let mut copy = self.clone();
        let notes = copy.sanitize();
        // Anything sanitize had to change (other than list normalisation) is an error.
        errs.extend(notes.into_iter().filter(|n| !n.contains("missing from configuration")));
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }

    /// Load from `path`. A missing file yields defaults; a malformed file is preserved as
    /// `<name>.corrupt-<n>` and defaults are used, so a bad edit never loses the original.
    pub fn load(path: &Path) -> Loaded {
        let mut notes = Vec::new();
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Loaded { config: Config::default(), notes: vec!["no configuration file, using defaults".into()] };
            }
            Err(e) => {
                notes.push(format!("could not read configuration ({e}), using defaults"));
                return Loaded { config: Config::default(), notes };
            }
        };
        match Self::from_json(&text) {
            Ok((cfg, mut n)) => {
                notes.append(&mut n);
                Loaded { config: cfg, notes }
            }
            Err(e) => {
                let backup = backup_name(path);
                match std::fs::rename(path, &backup) {
                    Ok(()) => notes.push(format!("configuration was malformed ({e}); moved to {} and using defaults", backup.display())),
                    Err(re) => notes.push(format!("configuration malformed ({e}) and backup failed ({re})")),
                }
                Loaded { config: Config::default(), notes }
            }
        }
    }

    /// Parse, migrate and sanitize JSON text.
    pub fn from_json(text: &str) -> Result<(Config, Vec<String>), String> {
        let mut value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut notes = migrate(&mut value)?;
        let mut cfg: Config = serde_json::from_value(value).map_err(|e| e.to_string())?;
        notes.extend(cfg.sanitize());
        Ok((cfg, notes))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("config serializes")
    }

    /// Crash-safe save: write a sibling temp file, flush it to disk, then atomically rename
    /// over the destination (rename replaces atomically on NTFS and POSIX filesystems).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(self.to_json().as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }
}

fn backup_name(path: &Path) -> PathBuf {
    for n in 1..1000 {
        let p = path.with_extension(format!("json.corrupt-{n}"));
        if !p.exists() {
            return p;
        }
    }
    path.with_extension("json.corrupt")
}

/// Upgrade older schema versions in place. Files from a *newer* version are accepted
/// (unknown fields ignored) but noted, because a downgrade may drop settings.
fn migrate(v: &mut serde_json::Value) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    let obj = v.as_object_mut().ok_or("configuration root is not an object")?;
    let ver = obj.get("schema_version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    if ver == 0 {
        // Pre-release files had no version and a flat `refresh_ms` field.
        if let Some(r) = obj.remove("refresh_ms") {
            let general = obj.entry("general").or_insert_with(|| serde_json::json!({}));
            if let Some(g) = general.as_object_mut() {
                g.insert("refresh_interval_ms".into(), r);
            }
        }
        notes.push("migrated configuration from unversioned format".into());
    } else if ver > SCHEMA_VERSION {
        notes.push(format!("configuration was written by a newer FreeHM (schema {ver}); unknown settings are ignored"));
    }
    obj.insert("schema_version".into(), SCHEMA_VERSION.into());
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("freehm-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn defaults_match_requirements() {
        let c = Config::default();
        assert_eq!(c.general.refresh_interval_ms, 200);
        assert_eq!(c.hotkeys.toggle_overlay, "Ctrl+Shift+O");
        assert!(c.general.start_with_system);
        assert_eq!(c.metrics.len(), MetricId::ALL.len());
        assert!(c.validate().is_ok());
    }

    #[test]
    fn round_trip_save_load() {
        let d = tmpdir("roundtrip");
        let p = d.join("config.json");
        let mut c = Config::default();
        c.general.refresh_interval_ms = 500;
        c.overlay.anchor = Anchor::BottomLeft;
        c.overlay.font = FontChoice::System { family: "Segoe UI".into(), path: None };
        c.detection.manual_games.push("mygame.exe".into());
        c.metrics.swap(0, 3);
        c.save(&p).unwrap();
        let l = Config::load(&p);
        assert_eq!(l.config, c);
        assert!(!p.with_extension("json.tmp").exists());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let d = tmpdir("missing");
        let l = Config::load(&d.join("nope.json"));
        assert_eq!(l.config, Config::default());
    }

    #[test]
    fn malformed_file_is_backed_up_and_defaults_used() {
        let d = tmpdir("malformed");
        let p = d.join("config.json");
        std::fs::write(&p, "{ this is not json").unwrap();
        let l = Config::load(&p);
        assert_eq!(l.config, Config::default());
        assert!(d.join("config.json.corrupt-1").exists());
        assert!(l.notes.iter().any(|n| n.contains("malformed")));
    }

    #[test]
    fn out_of_range_values_are_repaired() {
        let json = r#"{"schema_version":1,"general":{"refresh_interval_ms":5},
            "overlay":{"opacity":7.0,"font_size":-3},"hotkeys":{"toggle_overlay":"Banana+Q"}}"#;
        let (c, notes) = Config::from_json(json).unwrap();
        assert_eq!(c.general.refresh_interval_ms, MIN_REFRESH_MS);
        assert_eq!(c.overlay.opacity, 1.0);
        assert_eq!(c.overlay.font_size, 6.0);
        assert_eq!(c.hotkeys.toggle_overlay, DEFAULT_HOTKEY);
        assert!(notes.len() >= 4);
    }

    #[test]
    fn obsolete_and_unknown_fields_are_tolerated_and_migrated() {
        let json = r#"{"refresh_ms":1000,"some_removed_setting":true,"overlay":{"old_thing":1}}"#;
        let (c, notes) = Config::from_json(json).unwrap();
        assert_eq!(c.schema_version, SCHEMA_VERSION);
        assert_eq!(c.general.refresh_interval_ms, 1000);
        assert!(notes.iter().any(|n| n.contains("migrated")));
    }

    #[test]
    fn newer_schema_is_accepted_with_note() {
        let (c, notes) = Config::from_json(r#"{"schema_version":99}"#).unwrap();
        assert_eq!(c.schema_version, SCHEMA_VERSION);
        assert!(notes.iter().any(|n| n.contains("newer")));
    }

    #[test]
    fn metric_list_is_completed_and_deduplicated() {
        let json = r#"{"metrics":[{"id":"Fps","enabled":true},{"id":"Fps","enabled":false}]}"#;
        let (c, _) = Config::from_json(json).unwrap();
        assert_eq!(c.metrics.len(), MetricId::ALL.len());
        assert_eq!(c.metrics[0].id, MetricId::Fps);
        assert!(c.metrics[0].enabled);
        assert_eq!(c.enabled_metrics().count(), 1);
    }

    #[test]
    fn validate_rejects_bad_interval_and_hotkey() {
        let mut c = Config::default();
        c.general.refresh_interval_ms = 10;
        c.hotkeys.toggle_overlay = "Ctrl+".into();
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("Update interval")));
        assert!(errs.iter().any(|e| e.contains("Shortcut")));
    }

    #[test]
    fn hex_colors() {
        assert_eq!(Rgba::from_hex("#76B900"), Some(Rgba(0x76, 0xB9, 0, 255)));
        assert_eq!(Rgba::from_hex("00000080"), Some(Rgba(0, 0, 0, 0x80)));
        assert_eq!(Rgba::from_hex("xyz"), None);
        assert_eq!(Rgba(1, 2, 3, 4).to_hex(), "#01020304");
    }
}
