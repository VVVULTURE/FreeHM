//! The telemetry data model shared by providers, the overlay, the settings UI and IPC.
//!
//! Every value carries its unit, the device it belongs to, a freshness state and — when it
//! cannot be read — a human-readable reason. A missing sensor is never represented as `0`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Every statistic the overlay can display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MetricId {
    CpuUsage,
    CpuTemp,
    CpuClock,
    CpuPower,
    GpuUsage,
    GpuTemp,
    GpuClock,
    GpuPower,
    RamUsed,
    RamUsage,
    RamTotal,
    /// Dedicated video memory in use. Unavailable on GPUs without dedicated VRAM.
    VramUsed,
    VramUsage,
    VramTotal,
    /// Shared (system) memory currently allocated by the GPU. Distinct from VRAM.
    GpuSharedMem,
    Fps,
    FrameTime,
}

impl MetricId {
    pub const ALL: [MetricId; 17] = [
        MetricId::Fps,
        MetricId::FrameTime,
        MetricId::GpuUsage,
        MetricId::GpuTemp,
        MetricId::GpuClock,
        MetricId::GpuPower,
        MetricId::VramUsed,
        MetricId::VramUsage,
        MetricId::VramTotal,
        MetricId::GpuSharedMem,
        MetricId::CpuUsage,
        MetricId::CpuTemp,
        MetricId::CpuClock,
        MetricId::CpuPower,
        MetricId::RamUsed,
        MetricId::RamUsage,
        MetricId::RamTotal,
    ];

    pub fn unit(self) -> Unit {
        use MetricId::*;
        match self {
            CpuUsage | GpuUsage | RamUsage | VramUsage => Unit::Percent,
            CpuTemp | GpuTemp => Unit::Celsius,
            CpuClock | GpuClock => Unit::Megahertz,
            CpuPower | GpuPower => Unit::Watts,
            RamUsed | RamTotal | VramUsed | VramTotal | GpuSharedMem => Unit::Bytes,
            Fps => Unit::FramesPerSecond,
            FrameTime => Unit::Milliseconds,
        }
    }

    pub fn source(self) -> DeviceKind {
        use MetricId::*;
        match self {
            CpuUsage | CpuTemp | CpuClock | CpuPower => DeviceKind::Cpu,
            GpuUsage | GpuTemp | GpuClock | GpuPower | VramUsed | VramUsage | VramTotal | GpuSharedMem => DeviceKind::Gpu,
            RamUsed | RamUsage | RamTotal => DeviceKind::Memory,
            Fps | FrameTime => DeviceKind::Game,
        }
    }

    /// Full label used in the settings UI.
    pub fn display_name(self) -> &'static str {
        use MetricId::*;
        match self {
            CpuUsage => "CPU utilization",
            CpuTemp => "CPU temperature",
            CpuClock => "CPU clock",
            CpuPower => "CPU power",
            GpuUsage => "GPU utilization",
            GpuTemp => "GPU temperature",
            GpuClock => "GPU clock",
            GpuPower => "GPU power",
            RamUsed => "RAM used",
            RamUsage => "RAM utilization",
            RamTotal => "RAM total",
            VramUsed => "VRAM used",
            VramUsage => "VRAM utilization",
            VramTotal => "VRAM total",
            GpuSharedMem => "GPU shared memory",
            Fps => "Frame rate (FPS)",
            FrameTime => "Frame time",
        }
    }

    /// Default overlay label (normal presentation).
    pub fn default_label(self) -> &'static str {
        use MetricId::*;
        match self {
            CpuUsage => "CPU",
            CpuTemp => "CPU Temp",
            CpuClock => "CPU Clock",
            CpuPower => "CPU Power",
            GpuUsage => "GPU",
            GpuTemp => "GPU Temp",
            GpuClock => "GPU Clock",
            GpuPower => "GPU Power",
            RamUsed => "RAM",
            RamUsage => "RAM",
            RamTotal => "RAM Total",
            VramUsed => "VRAM",
            VramUsage => "VRAM",
            VramTotal => "VRAM Total",
            GpuSharedMem => "GPU Shared",
            Fps => "FPS",
            FrameTime => "Frame Time",
        }
    }

    /// Short label for the compact presentation.
    pub fn compact_label(self) -> &'static str {
        use MetricId::*;
        match self {
            CpuUsage => "CPU",
            CpuTemp => "CPU",
            CpuClock => "CPU",
            CpuPower => "CPU",
            GpuUsage => "GPU",
            GpuTemp => "GPU",
            GpuClock => "GPU",
            GpuPower => "GPU",
            RamUsed | RamUsage | RamTotal => "RAM",
            VramUsed | VramUsage | VramTotal => "VRAM",
            GpuSharedMem => "SHR",
            Fps => "FPS",
            FrameTime => "FT",
        }
    }

    /// Default group used by the grouped layout.
    pub fn default_group(self) -> &'static str {
        match self.source() {
            DeviceKind::Cpu => "CPU",
            DeviceKind::Gpu => match self {
                MetricId::VramUsed | MetricId::VramUsage | MetricId::VramTotal => "VRAM",
                _ => "GPU",
            },
            DeviceKind::Memory => "RAM",
            DeviceKind::Game => "FPS",
        }
    }

    /// Plausible physical range. Values outside it are treated as sensor faults rather than
    /// displayed, so a broken sensor reporting e.g. 6553.5 °C never reaches the overlay.
    pub fn valid_range(self) -> (f64, f64) {
        use MetricId::*;
        match self {
            CpuUsage | GpuUsage | RamUsage | VramUsage => (0.0, 100.5),
            CpuTemp | GpuTemp => (-30.0, 150.0),
            CpuClock | GpuClock => (0.0, 20_000.0),
            CpuPower | GpuPower => (0.0, 2_000.0),
            RamUsed | RamTotal | VramUsed | VramTotal | GpuSharedMem => (0.0, 1.0e15),
            Fps => (0.0, 10_000.0),
            FrameTime => (0.0, 60_000.0),
        }
    }
}

impl fmt::Display for MetricId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    Percent,
    Celsius,
    Megahertz,
    Watts,
    Bytes,
    FramesPerSecond,
    Milliseconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeviceKind {
    Cpu,
    Gpu,
    Memory,
    Game,
}

/// Freshness of a reading, as defined in the architecture document:
/// - `Fresh`: read successfully during the most recent poll.
/// - `Cached`: deliberately not re-read this poll because the source updates more slowly
///   than the poll interval; the last good value is shown.
/// - `Stale`: the most recent attempts failed; the value is the last good one and is older
///   than the staleness threshold. The overlay dims it.
/// - `Unavailable`: there is no value (unsupported, permission denied, never read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Freshness {
    Fresh,
    Cached,
    Stale,
    Unavailable,
}

/// One metric value as presented to consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub metric: MetricId,
    pub device: String,
    /// `None` exactly when `freshness == Unavailable`.
    pub value: Option<f64>,
    pub unit: Unit,
    pub freshness: Freshness,
    /// Milliseconds since the value was obtained from the source (None when unavailable).
    pub age_ms: Option<u64>,
    /// Why the value is unavailable or stale.
    pub reason: Option<String>,
    /// Name of the provider that produced it (diagnostics).
    pub source: String,
}

impl Reading {
    pub fn unavailable(metric: MetricId, device: &str, source: &str, reason: impl Into<String>) -> Self {
        Reading {
            metric,
            device: device.to_string(),
            value: None,
            unit: metric.unit(),
            freshness: Freshness::Unavailable,
            age_ms: None,
            reason: Some(reason.into()),
            source: source.to_string(),
        }
    }

    pub fn is_available(&self) -> bool {
        self.value.is_some() && self.freshness != Freshness::Unavailable
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Stable identifier, e.g. `gpu:nvml:0000:01:00.0` or `cpu:0`.
    pub id: String,
    pub kind: DeviceKind,
    pub name: String,
    pub vendor: Vendor,
    /// True for integrated GPUs / APUs that use shared system memory instead of VRAM.
    pub integrated: bool,
    /// Provider that enumerated it.
    pub provider: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vendor {
    Intel,
    Amd,
    Nvidia,
    Other,
}

impl Vendor {
    pub fn from_pci_id(id: u32) -> Vendor {
        match id {
            0x8086 => Vendor::Intel,
            0x1002 | 0x1022 => Vendor::Amd,
            0x10de => Vendor::Nvidia,
            _ => Vendor::Other,
        }
    }
}

/// Everything the telemetry engine knows at one instant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub devices: Vec<DeviceInfo>,
    pub readings: Vec<Reading>,
    /// Provider-level problems ("NVML not found", "helper not installed"), for diagnostics.
    pub provider_status: Vec<ProviderStatus>,
    /// Monotonic sequence number, incremented on every published snapshot.
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub provider: String,
    pub ok: bool,
    pub message: String,
}

impl Snapshot {
    pub fn get(&self, device: &str, metric: MetricId) -> Option<&Reading> {
        self.readings.iter().find(|r| r.metric == metric && r.device == device)
    }

    pub fn devices_of(&self, kind: DeviceKind) -> impl Iterator<Item = &DeviceInfo> {
        self.devices.iter().filter(move |d| d.kind == kind)
    }

    /// Resolve which device supplies a metric, honouring the user's CPU/GPU selection.
    /// With no (or a vanished) selection, GPUs prefer a discrete device.
    pub fn device_for(&self, kind: DeviceKind, selected: Option<&str>) -> Option<&DeviceInfo> {
        if let Some(sel) = selected
            && let Some(d) = self.devices.iter().find(|d| d.kind == kind && d.id == sel)
        {
            return Some(d);
        }
        let mut it = self.devices_of(kind);
        match kind {
            DeviceKind::Gpu => {
                let all: Vec<&DeviceInfo> = it.collect();
                all.iter().find(|d| !d.integrated).copied().or_else(|| all.first().copied())
            }
            _ => it.next(),
        }
    }
}

/// Well-known device ids for singleton devices.
pub const DEVICE_MEMORY: &str = "mem:system";
pub const DEVICE_GAME: &str = "game:active";

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, kind: DeviceKind, integrated: bool) -> DeviceInfo {
        DeviceInfo { id: id.into(), kind, name: id.into(), vendor: Vendor::Other, integrated, provider: "test".into() }
    }

    #[test]
    fn every_metric_listed_once() {
        let mut v = MetricId::ALL.to_vec();
        v.sort();
        v.dedup();
        assert_eq!(v.len(), MetricId::ALL.len());
    }

    #[test]
    fn gpu_selection_prefers_discrete_then_honours_choice() {
        let s = Snapshot {
            devices: vec![dev("gpu:igpu", DeviceKind::Gpu, true), dev("gpu:dgpu", DeviceKind::Gpu, false), dev("cpu:0", DeviceKind::Cpu, false)],
            ..Default::default()
        };
        assert_eq!(s.device_for(DeviceKind::Gpu, None).unwrap().id, "gpu:dgpu");
        assert_eq!(s.device_for(DeviceKind::Gpu, Some("gpu:igpu")).unwrap().id, "gpu:igpu");
        // A selection that no longer exists falls back to automatic choice.
        assert_eq!(s.device_for(DeviceKind::Gpu, Some("gpu:gone")).unwrap().id, "gpu:dgpu");
        assert_eq!(s.device_for(DeviceKind::Cpu, None).unwrap().id, "cpu:0");
    }

    #[test]
    fn unavailable_reading_has_no_value() {
        let r = Reading::unavailable(MetricId::GpuPower, "gpu:x", "test", "not exposed");
        assert!(!r.is_available());
        assert_eq!(r.value, None);
        assert_eq!(r.unit, Unit::Watts);
    }

    #[test]
    fn pci_vendor_mapping() {
        assert_eq!(Vendor::from_pci_id(0x10de), Vendor::Nvidia);
        assert_eq!(Vendor::from_pci_id(0x1002), Vendor::Amd);
        assert_eq!(Vendor::from_pci_id(0x8086), Vendor::Intel);
        assert_eq!(Vendor::from_pci_id(0x1234), Vendor::Other);
    }
}
