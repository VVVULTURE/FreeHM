//! Turning readings into overlay text: units, conversions and precision.

use crate::config::{Config, TemperatureUnit};
use crate::metrics::{Freshness, MetricId, Reading};

pub const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
pub const MIB: f64 = 1024.0 * 1024.0;

/// Text placeholder for a metric with no value. Deliberately not "0".
pub const UNAVAILABLE: &str = "N/A";

#[derive(Debug, Clone, PartialEq)]
pub struct Formatted {
    pub value: String,
    pub unit: String,
    /// The value is old/stale and should be drawn dimmed.
    pub dim: bool,
    pub available: bool,
}

pub fn celsius_to_fahrenheit(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// Pick binary GB for >= 1 GiB, MB otherwise; returns (value, unit).
pub fn bytes_scaled(bytes: f64) -> (f64, &'static str) {
    if bytes >= GIB { (bytes / GIB, "GB") } else { (bytes / MIB, "MB") }
}

fn fixed(v: f64, decimals: usize) -> String {
    // Avoid "-0" for tiny negative noise.
    let v = if v.abs() < 0.5 * 10f64.powi(-(decimals as i32)) { 0.0 } else { v };
    format!("{v:.decimals$}")
}

/// Format one reading according to the user's unit preferences.
/// `total` is the matching total reading when "used / total" display is requested.
pub fn format_reading(r: &Reading, cfg: &Config, total: Option<&Reading>) -> Formatted {
    let Some(v) = r.value.filter(|_| r.freshness != Freshness::Unavailable) else {
        return Formatted { value: UNAVAILABLE.into(), unit: String::new(), dim: true, available: false };
    };
    let dim = r.freshness == Freshness::Stale;
    let (value, unit) = format_value(r.metric, v, cfg, total.and_then(|t| t.value));
    Formatted { value, unit, dim, available: true }
}

pub fn format_value(metric: MetricId, v: f64, cfg: &Config, total: Option<f64>) -> (String, String) {
    use MetricId::*;
    match metric {
        CpuUsage | GpuUsage | RamUsage | VramUsage => (fixed(v, 0), "%".into()),
        CpuTemp | GpuTemp => match cfg.general.temperature_unit {
            TemperatureUnit::Celsius => (fixed(v, 0), "°C".into()),
            TemperatureUnit::Fahrenheit => (fixed(celsius_to_fahrenheit(v), 0), "°F".into()),
        },
        CpuClock | GpuClock => {
            if cfg.general.clock_in_ghz {
                (fixed(v / 1000.0, 2), "GHz".into())
            } else {
                (fixed(v, 0), "MHz".into())
            }
        }
        CpuPower | GpuPower => (fixed(v, if v < 10.0 { 1 } else { 0 }), "W".into()),
        RamUsed | RamTotal | VramUsed | VramTotal | GpuSharedMem => {
            // Use the total's scale for both numbers so "7.9 / 12.0 GB" stays consistent.
            let (_, unit) = bytes_scaled(total.unwrap_or(v).max(v));
            let div = if unit == "GB" { GIB } else { MIB };
            let dec = if unit == "GB" { 1 } else { 0 };
            match total {
                Some(t) if t > 0.0 => (format!("{} / {}", fixed(v / div, dec), fixed(t / div, dec)), unit.into()),
                _ => (fixed(v / div, dec), unit.into()),
            }
        }
        Fps | FpsLow1 | FpsLow01 => (fixed(v, 0), String::new()),
        FrameTime => (fixed(v, 1), "ms".into()),
    }
}

/// Unit shown in the settings UI next to a raw reading (no user preferences applied).
pub fn raw_unit(metric: MetricId) -> &'static str {
    use crate::metrics::Unit::*;
    match metric.unit() {
        Percent => "%",
        Celsius => "°C",
        Megahertz => "MHz",
        Watts => "W",
        Bytes => "bytes",
        FramesPerSecond => "FPS",
        Milliseconds => "ms",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{Freshness, Reading};

    fn reading(metric: MetricId, v: f64) -> Reading {
        Reading {
            metric,
            device: "d".into(),
            value: Some(v),
            unit: metric.unit(),
            freshness: Freshness::Fresh,
            age_ms: Some(0),
            reason: None,
            source: "t".into(),
        }
    }

    #[test]
    fn temperature_conversion() {
        assert_eq!(celsius_to_fahrenheit(0.0), 32.0);
        assert_eq!(celsius_to_fahrenheit(100.0), 212.0);
        let mut c = Config::default();
        assert_eq!(format_value(MetricId::GpuTemp, 65.4, &c, None), ("65".into(), "°C".into()));
        c.general.temperature_unit = TemperatureUnit::Fahrenheit;
        assert_eq!(format_value(MetricId::GpuTemp, 65.4, &c, None), ("150".into(), "°F".into()));
    }

    #[test]
    fn clocks_mhz_and_ghz() {
        let mut c = Config::default();
        assert_eq!(format_value(MetricId::CpuClock, 4062.4, &c, None).0, "4062");
        c.general.clock_in_ghz = true;
        assert_eq!(format_value(MetricId::CpuClock, 4062.4, &c, None), ("4.06".into(), "GHz".into()));
    }

    #[test]
    fn memory_used_and_total_share_scale() {
        let c = Config::default();
        let (v, u) = format_value(MetricId::VramUsed, 512.0 * MIB, &c, Some(12.0 * GIB));
        assert_eq!((v.as_str(), u.as_str()), ("0.5 / 12.0", "GB"));
        let (v, u) = format_value(MetricId::RamUsed, 300.0 * MIB, &c, None);
        assert_eq!((v.as_str(), u.as_str()), ("300", "MB"));
        let (v, _) = format_value(MetricId::RamTotal, 31.7 * GIB, &c, None);
        assert_eq!(v, "31.7");
    }

    #[test]
    fn power_precision() {
        let c = Config::default();
        assert_eq!(format_value(MetricId::GpuPower, 7.25, &c, None).0, "7.2");
        assert_eq!(format_value(MetricId::GpuPower, 245.6, &c, None).0, "246");
    }

    #[test]
    fn zero_is_shown_as_zero_but_unavailable_is_na() {
        let c = Config::default();
        let f = format_reading(&reading(MetricId::GpuUsage, 0.0), &c, None);
        assert_eq!(f.value, "0");
        assert!(f.available);
        let na = format_reading(&Reading::unavailable(MetricId::GpuPower, "d", "t", "x"), &c, None);
        assert_eq!(na.value, UNAVAILABLE);
        assert!(!na.available);
    }

    #[test]
    fn stale_values_are_dimmed() {
        let mut r = reading(MetricId::CpuTemp, 50.0);
        r.freshness = Freshness::Stale;
        assert!(format_reading(&r, &Config::default(), None).dim);
    }

    #[test]
    fn no_negative_zero() {
        assert_eq!(format_value(MetricId::CpuUsage, -0.2, &Config::default(), None).0, "0");
    }
}
