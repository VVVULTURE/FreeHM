//! Unprivileged Linux telemetry providers (procfs/sysfs).
//!
//! - CPU: `/proc/stat` (utilisation), `cpufreq` (clock), hwmon `coretemp`/`k10temp`/
//!   `zenpower` (temperature), powercap RAPL `energy_uj` (power; root-only on kernels
//!   since 5.10 unless the one-time permission setup was run).
//! - Memory: `/proc/meminfo` (MemTotal − MemAvailable).
//! - GPUs via DRM sysfs: amdgpu (busy %, VRAM/GTT, hwmon temperature/power/clock), Intel
//!   i915/xe (clock; temperature/power where hwmon exposes them). NVIDIA values come from
//!   NVML (`sensors::nvml`), which takes precedence.
//! - FPS/frame time: no system-wide, non-invasive source exists on Linux; reported as
//!   unavailable with the reason.

use crate::sensors::nvml::pci_device_id;
use freehm_core::metrics::{DEVICE_GAME, DEVICE_MEMORY, DeviceInfo, DeviceKind, MetricId, Vendor};
use freehm_core::telemetry::{PollRequest, Provider, Sample};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const CPU_DEVICE: &str = "cpu:0";

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_num(p: &Path) -> Option<f64> {
    read_trim(p)?.parse().ok()
}

/// `/sys/class/hwmon/hwmon*` directories with their `name`.
fn hwmons() -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir("/sys/class/hwmon") {
        for e in rd.flatten() {
            let p = e.path();
            if let Some(n) = read_trim(&p.join("name")) {
                v.push((n, p));
            }
        }
    }
    v
}

/// Find `temp*_input` whose `temp*_label` is one of `labels` (in preference order).
fn temp_by_label(dir: &Path, labels: &[&str]) -> Option<PathBuf> {
    for want in labels {
        for i in 1..=64 {
            let l = dir.join(format!("temp{i}_label"));
            if read_trim(&l).as_deref() == Some(*want) {
                return Some(dir.join(format!("temp{i}_input")));
            }
        }
    }
    None
}

pub fn cpu_name() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string()))
        .unwrap_or_else(|| "CPU".into())
}

fn cpu_vendor() -> Vendor {
    let s = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    if s.contains("GenuineIntel") {
        Vendor::Intel
    } else if s.contains("AuthenticAMD") {
        Vendor::Amd
    } else {
        Vendor::Other
    }
}

/// (busy, total) jiffies from the aggregate line of `/proc/stat`.
pub fn parse_proc_stat(s: &str) -> Option<(u64, u64)> {
    let line = s.lines().next()?;
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    let v: Vec<u64> = it.take(8).filter_map(|x| x.parse().ok()).collect();
    if v.len() < 4 {
        return None;
    }
    let total: u64 = v.iter().sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0); // idle + iowait
    Some((total - idle, total))
}

/// (total, available) bytes from `/proc/meminfo`.
pub fn parse_meminfo(s: &str) -> Option<(f64, f64)> {
    let get = |key: &str| -> Option<f64> {
        let l = s.lines().find(|l| l.starts_with(key))?;
        let kb: f64 = l.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024.0)
    };
    Some((get("MemTotal:")?, get("MemAvailable:")?))
}

struct Rapl {
    energy: PathBuf,
    max_range: f64,
    prev: Option<(f64, Instant)>,
}

pub struct CpuProvider {
    prev_stat: Option<(u64, u64)>,
    temp: Result<(PathBuf, String), String>,
    freq_files: Vec<PathBuf>,
    rapl: Result<Rapl, String>,
    rapl_checked: Instant,
}

impl CpuProvider {
    pub fn new() -> CpuProvider {
        CpuProvider {
            prev_stat: None,
            temp: Err("not initialised".into()),
            freq_files: Vec::new(),
            rapl: Err("not initialised".into()),
            rapl_checked: Instant::now(),
        }
    }

    fn find_temp() -> Result<(PathBuf, String), String> {
        for (name, dir) in hwmons() {
            let found = match name.as_str() {
                "coretemp" => temp_by_label(&dir, &["Package id 0"]).map(|p| (p, "coretemp package")),
                "k10temp" => temp_by_label(&dir, &["Tdie", "Tctl"]).map(|p| (p, "k10temp")),
                "zenpower" => temp_by_label(&dir, &["Tdie", "Tctl"]).map(|p| (p, "zenpower")),
                "cpu_thermal" => Some((dir.join("temp1_input"), "cpu_thermal")),
                _ => None,
            };
            if let Some((p, src)) = found
                && p.exists()
            {
                return Ok((p, src.to_string()));
            }
        }
        if let Ok(rd) = fs::read_dir("/sys/class/thermal") {
            for e in rd.flatten() {
                if read_trim(&e.path().join("type")).as_deref() == Some("x86_pkg_temp") {
                    return Ok((e.path().join("temp"), "x86_pkg_temp thermal zone".into()));
                }
            }
        }
        Err("no CPU temperature sensor found (load the coretemp or k10temp kernel module)".into())
    }

    fn find_rapl() -> Result<Rapl, String> {
        let base = Path::new("/sys/class/powercap/intel-rapl:0");
        if !base.exists() {
            return Err("this system exposes no RAPL package energy counter (powercap intel-rapl)".into());
        }
        let energy = base.join("energy_uj");
        if fs::read_to_string(&energy).is_err() {
            if super::permissions::rule_installed() {
                return Err("the sensor permission setup has been run; log out and back in so this session joins the 'freehm' group".into());
            }
            return Err(
                "CPU power needs the one-time sensor permission setup (Settings ▸ Startup & permissions); the kernel restricts RAPL energy counters to root"
                    .into(),
            );
        }
        let max_range = read_num(&base.join("max_energy_range_uj")).unwrap_or(f64::MAX);
        Ok(Rapl { energy, max_range, prev: None })
    }
}

impl Provider for CpuProvider {
    fn name(&self) -> &'static str {
        "Linux procfs/sysfs (CPU)"
    }

    fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
        let stat = fs::read_to_string("/proc/stat").map_err(|e| format!("/proc/stat: {e}"))?;
        self.prev_stat = parse_proc_stat(&stat);
        self.temp = Self::find_temp();
        self.rapl = Self::find_rapl();
        self.rapl_checked = Instant::now();
        self.freq_files = fs::read_dir("/sys/devices/system/cpu")
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_name().to_string_lossy().strip_prefix("cpu").is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty()))
                    .map(|e| e.path().join("cpufreq/scaling_cur_freq"))
                    .filter(|p| p.exists())
                    .collect()
            })
            .unwrap_or_default();
        Ok(vec![DeviceInfo {
            id: CPU_DEVICE.into(),
            kind: DeviceKind::Cpu,
            name: cpu_name(),
            vendor: cpu_vendor(),
            integrated: false,
            provider: self.name().into(),
        }])
    }

    fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String> {
        let m = &req.metrics;
        if m.contains(MetricId::CpuUsage) {
            let stat = fs::read_to_string("/proc/stat").map_err(|e| format!("/proc/stat: {e}"))?;
            if let Some(cur) = parse_proc_stat(&stat) {
                if let Some(prev) = self.prev_stat
                    && cur.1 > prev.1
                {
                    let busy = cur.0.saturating_sub(prev.0) as f64;
                    out.push(Sample::value(CPU_DEVICE, MetricId::CpuUsage, busy / (cur.1 - prev.1) as f64 * 100.0));
                }
                self.prev_stat = Some(cur);
            }
        }
        if m.contains(MetricId::CpuClock) {
            let f: Vec<f64> = self.freq_files.iter().filter_map(|p| read_num(p)).collect();
            if f.is_empty() {
                out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuClock, "cpufreq is not available (virtual machine or cpufreq driver missing)"));
            } else {
                // Mean of the per-core current frequencies (kHz → MHz).
                out.push(Sample::value(CPU_DEVICE, MetricId::CpuClock, f.iter().sum::<f64>() / f.len() as f64 / 1000.0));
            }
        }
        if m.contains(MetricId::CpuTemp) {
            match &self.temp {
                Ok((p, _)) => match read_num(p) {
                    Some(mc) => out.push(Sample::value(CPU_DEVICE, MetricId::CpuTemp, mc / 1000.0)),
                    None => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuTemp, "temperature sensor read failed")),
                },
                Err(r) => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuTemp, r.clone())),
            }
        }
        if m.contains(MetricId::CpuPower) {
            if self.rapl.is_err() && self.rapl_checked.elapsed().as_secs() >= 10 {
                // Permissions may have been granted since (setup run): look again, rarely.
                self.rapl = Self::find_rapl();
                self.rapl_checked = Instant::now();
            }
            match &mut self.rapl {
                Ok(r) => {
                    if let Some(uj) = read_num(&r.energy) {
                        let now = Instant::now();
                        if let Some((pu, pt)) = r.prev {
                            let mut d = uj - pu;
                            if d < 0.0 {
                                d += r.max_range;
                            }
                            let dt = now.duration_since(pt).as_secs_f64();
                            if dt > 0.05 {
                                out.push(Sample::value(CPU_DEVICE, MetricId::CpuPower, d / 1e6 / dt));
                                r.prev = Some((uj, now));
                            }
                        } else {
                            r.prev = Some((uj, now));
                        }
                    }
                }
                Err(e) => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuPower, e.clone())),
            }
        }
        Ok(())
    }
}

pub struct MemoryProvider;

impl Provider for MemoryProvider {
    fn name(&self) -> &'static str {
        "Linux /proc/meminfo"
    }

    fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
        Ok(vec![DeviceInfo {
            id: DEVICE_MEMORY.into(),
            kind: DeviceKind::Memory,
            name: "System memory".into(),
            vendor: Vendor::Other,
            integrated: false,
            provider: self.name().into(),
        }])
    }

    fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String> {
        if !req.metrics.any_of(&[MetricId::RamUsed, MetricId::RamUsage, MetricId::RamTotal]) {
            return Ok(());
        }
        let s = fs::read_to_string("/proc/meminfo").map_err(|e| format!("/proc/meminfo: {e}"))?;
        let (total, avail) = parse_meminfo(&s).ok_or("unexpected /proc/meminfo format")?;
        let used = total - avail;
        out.push(Sample::value(DEVICE_MEMORY, MetricId::RamTotal, total));
        out.push(Sample::value(DEVICE_MEMORY, MetricId::RamUsed, used));
        out.push(Sample::value(DEVICE_MEMORY, MetricId::RamUsage, used / total * 100.0));
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------

struct Gpu {
    id: String,
    vendor: Vendor,
    dev: PathBuf,
    hwmon: Option<PathBuf>,
    driver: String,
    vram_total: Option<f64>,
    energy_prev: Option<(f64, Instant)>,
}

/// GPUs from `/sys/class/drm/card*` (render-capable PCI devices).
pub struct DrmGpuProvider {
    gpus: Vec<Gpu>,
}

impl DrmGpuProvider {
    pub fn new() -> DrmGpuProvider {
        DrmGpuProvider { gpus: Vec::new() }
    }
}

fn pci_from_dev(dev: &Path) -> Option<(u32, u32, u32)> {
    // ".../0000:03:00.0"
    let real = fs::canonicalize(dev).ok()?;
    let name = real.file_name()?.to_string_lossy().to_string();
    let mut p = name.split([':', '.']);
    let _domain = p.next()?;
    let bus = u32::from_str_radix(p.next()?, 16).ok()?;
    let d = u32::from_str_radix(p.next()?, 16).ok()?;
    let f = u32::from_str_radix(p.next()?, 16).ok()?;
    Some((bus, d, f))
}

fn gpu_name(dev: &Path, vendor: Vendor, driver: &str) -> String {
    // The kernel has no marketing name; use the vendor/device ids unless a product name exists.
    if let Some(n) = read_trim(&dev.join("product_name")).filter(|n| !n.is_empty()) {
        return n;
    }
    let did = read_trim(&dev.join("device")).unwrap_or_default();
    let v = match vendor {
        Vendor::Amd => "AMD Radeon",
        Vendor::Intel => "Intel Graphics",
        Vendor::Nvidia => "NVIDIA GPU",
        Vendor::Other => "GPU",
    };
    format!("{v} ({driver} {did})")
}

impl Provider for DrmGpuProvider {
    fn name(&self) -> &'static str {
        "Linux DRM sysfs (GPU)"
    }

    fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
        self.gpus.clear();
        let mut infos = Vec::new();
        let rd = fs::read_dir("/sys/class/drm").map_err(|e| format!("/sys/class/drm: {e}"))?;
        let mut cards: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().strip_prefix("card").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
            .map(|e| e.path())
            .collect();
        cards.sort();
        for card in cards {
            let dev = card.join("device");
            let Some(vendor_id) = read_trim(&dev.join("vendor")).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()) else { continue };
            let Some((bus, d, f)) = pci_from_dev(&dev) else { continue };
            let id = pci_device_id(bus, d, f);
            if self.gpus.iter().any(|g| g.id == id) {
                continue;
            }
            let vendor = Vendor::from_pci_id(vendor_id);
            let driver = fs::read_link(dev.join("driver")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())).unwrap_or_default();
            let hwmon = fs::read_dir(dev.join("hwmon")).ok().and_then(|mut rd| rd.next()).and_then(|e| e.ok()).map(|e| e.path());
            let vram_total = read_num(&dev.join("mem_info_vram_total"));
            let integrated = match vendor {
                // Intel integrated graphics sit on the root bus (00:02.0); Arc cards do not.
                Vendor::Intel => bus == 0,
                // APUs carve out a small framebuffer and use GTT (system RAM) for the rest.
                Vendor::Amd => vram_total.is_some_and(|t| t <= 1024.0 * 1024.0 * 1024.0),
                _ => false,
            };
            infos.push(DeviceInfo {
                id: id.clone(),
                kind: DeviceKind::Gpu,
                name: gpu_name(&dev, vendor, &driver),
                vendor,
                integrated,
                provider: "Linux DRM sysfs (GPU)".into(),
            });
            self.gpus.push(Gpu { id, vendor, dev, hwmon, driver, vram_total, energy_prev: None });
        }
        Ok(infos)
    }

    fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String> {
        let m = &req.metrics;
        let wanted = [
            MetricId::GpuUsage,
            MetricId::GpuTemp,
            MetricId::GpuClock,
            MetricId::GpuPower,
            MetricId::VramUsed,
            MetricId::VramUsage,
            MetricId::VramTotal,
            MetricId::GpuSharedMem,
        ];
        if !m.any_of(&wanted) {
            return Ok(());
        }
        for g in &mut self.gpus {
            let id = g.id.as_str();
            if g.vendor == Vendor::Nvidia {
                for metric in wanted.iter().copied().filter(|x| m.contains(*x)) {
                    out.push(Sample::unavailable(
                        id,
                        metric,
                        format!("NVIDIA values come from the NVIDIA driver's NVML library (driver in use: {})", g.driver),
                    ));
                }
                continue;
            }
            let hw = g.hwmon.clone();
            let hwnum = |f: &str| hw.as_ref().and_then(|h| read_num(&h.join(f)));
            if m.contains(MetricId::GpuUsage) {
                match read_num(&g.dev.join("gpu_busy_percent")) {
                    Some(v) => out.push(Sample::value(id, MetricId::GpuUsage, v)),
                    None => out.push(Sample::unavailable(
                        id,
                        MetricId::GpuUsage,
                        if g.vendor == Vendor::Intel {
                            "the Intel driver exposes GPU busy time only through perf counters, which need extra privileges"
                        } else {
                            "the driver does not report GPU utilisation"
                        },
                    )),
                }
            }
            if m.contains(MetricId::GpuTemp) {
                let t = hw.as_ref().and_then(|h| temp_by_label(h, &["edge"]).or_else(|| Some(h.join("temp1_input")))).and_then(|p| read_num(&p));
                match t {
                    Some(mc) => out.push(Sample::value(id, MetricId::GpuTemp, mc / 1000.0)),
                    None => out.push(Sample::unavailable(id, MetricId::GpuTemp, "the driver exposes no GPU temperature sensor")),
                }
            }
            if m.contains(MetricId::GpuClock) {
                let mhz = hwnum("freq1_input")
                    .map(|hz| hz / 1e6)
                    .or_else(|| read_num(&g.dev.join("drm").join(format!("card{}", card_index(&g.dev).unwrap_or(0))).join("gt_act_freq_mhz")))
                    .or_else(|| read_num(&g.dev.join("tile0/gt0/freq0/act_freq")))
                    .or_else(|| intel_act_freq(&g.dev));
                match mhz {
                    Some(v) => out.push(Sample::value(id, MetricId::GpuClock, v)),
                    None => out.push(Sample::unavailable(id, MetricId::GpuClock, "the driver does not report the current GPU clock")),
                }
            }
            if m.contains(MetricId::GpuPower) {
                if let Some(uw) = hwnum("power1_average").or_else(|| hwnum("power1_input")) {
                    out.push(Sample::value(id, MetricId::GpuPower, uw / 1e6));
                } else if let Some(uj) = hwnum("energy1_input") {
                    let now = Instant::now();
                    if let Some((p, t)) = g.energy_prev
                        && uj >= p
                        && now.duration_since(t).as_secs_f64() > 0.05
                    {
                        out.push(Sample::value(id, MetricId::GpuPower, (uj - p) / 1e6 / now.duration_since(t).as_secs_f64()));
                    }
                    g.energy_prev = Some((uj, now));
                } else {
                    out.push(Sample::unavailable(id, MetricId::GpuPower, "the driver does not report GPU power"));
                }
            }
            let used = read_num(&g.dev.join("mem_info_vram_used"));
            if m.any_of(&[MetricId::VramUsed, MetricId::VramUsage, MetricId::VramTotal]) {
                match (used, g.vram_total) {
                    (Some(u), Some(t)) if t > 0.0 => {
                        out.push(Sample::value(id, MetricId::VramUsed, u));
                        out.push(Sample::value(id, MetricId::VramTotal, t));
                        out.push(Sample::value(id, MetricId::VramUsage, u / t * 100.0));
                    }
                    _ => {
                        let r = if g.vendor == Vendor::Intel {
                            "integrated or Intel GPU: no dedicated VRAM counters are exposed"
                        } else {
                            "the driver does not report VRAM usage"
                        };
                        for x in [MetricId::VramUsed, MetricId::VramUsage, MetricId::VramTotal] {
                            out.push(Sample::unavailable(id, x, r));
                        }
                    }
                }
            }
            if m.contains(MetricId::GpuSharedMem) {
                match read_num(&g.dev.join("mem_info_gtt_used")) {
                    Some(v) => out.push(Sample::value(id, MetricId::GpuSharedMem, v)),
                    None => out.push(Sample::unavailable(id, MetricId::GpuSharedMem, "the driver does not report shared (GTT) memory use")),
                }
            }
        }
        Ok(())
    }
}

fn card_index(dev: &Path) -> Option<u32> {
    fs::read_dir(dev.join("drm")).ok()?.flatten().find_map(|e| e.file_name().to_string_lossy().strip_prefix("card").and_then(|n| n.parse().ok()))
}

fn intel_act_freq(dev: &Path) -> Option<f64> {
    let card = dev.join("drm").join(format!("card{}", card_index(dev)?));
    read_num(&card.join("gt/gt0/rps_act_freq_mhz"))
}

/// Linux has no system-wide, non-invasive frame timing source (unlike Windows ETW): FPS
/// counters there work by injecting a Vulkan/OpenGL layer into the game. Say so plainly.
pub struct NoFramesProvider;

impl Provider for NoFramesProvider {
    fn name(&self) -> &'static str {
        "Frame timing (Linux)"
    }
    fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
        Ok(Vec::new())
    }
    fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String> {
        const WHY: &str = "not available on Linux without loading code into the game (FreeHM does not); the game's own counter or MangoHud, a Vulkan/OpenGL layer, can show it";
        for m in [MetricId::Fps, MetricId::FrameTime] {
            if req.metrics.contains(m) {
                out.push(Sample::unavailable(DEVICE_GAME, m, WHY));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_parsing() {
        let s = "cpu  100 5 50 800 20 0 5 0 0 0\ncpu0 1 2 3 4\n";
        let (busy, total) = parse_proc_stat(s).unwrap();
        assert_eq!(total, 980);
        assert_eq!(busy, 980 - 820);
        assert!(parse_proc_stat("intr 1 2 3").is_none());
    }

    #[test]
    fn meminfo_parsing() {
        let s = "MemTotal:       32768000 kB\nMemFree:         1000000 kB\nMemAvailable:   16384000 kB\n";
        let (t, a) = parse_meminfo(s).unwrap();
        assert_eq!(t, 32768000.0 * 1024.0);
        assert_eq!(a, 16384000.0 * 1024.0);
    }
}
