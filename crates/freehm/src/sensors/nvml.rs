//! NVIDIA GPUs via NVML (NVIDIA Management Library), loaded at run time from the driver
//! installation (`nvml.dll` in System32 on Windows, `libnvidia-ml.so.1` on Linux). NVML is
//! NVIDIA's supported monitoring API, needs no elevation, and is not redistributed by FreeHM.

use freehm_core::metrics::{DeviceInfo, DeviceKind, MetricId, Vendor};
use freehm_core::telemetry::{PollRequest, Provider, Sample};
use libloading::Library;
use std::ffi::{CStr, c_char, c_uint, c_void};

type Ret = c_uint;
type Dev = *mut c_void;

const NVML_SUCCESS: Ret = 0;
const NVML_ERROR_NOT_SUPPORTED: Ret = 3;
const NVML_ERROR_NO_PERMISSION: Ret = 4;
const NVML_ERROR_GPU_IS_LOST: Ret = 15;
const NVML_ERROR_FUNCTION_NOT_FOUND: Ret = 13;
const NVML_ERROR_DRIVER_NOT_LOADED: Ret = 9;
const NVML_TEMPERATURE_GPU: c_uint = 0;
const NVML_CLOCK_GRAPHICS: c_uint = 0;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct MemoryV1 {
    total: u64,
    free: u64,
    used: u64,
}

/// nvmlMemory_v2_t: separates driver-reserved memory from memory used by applications.
#[repr(C)]
#[derive(Default)]
struct MemoryV2 {
    version: c_uint,
    total: u64,
    reserved: u64,
    free: u64,
    used: u64,
}
const NVML_MEMORY_V2: c_uint = (std::mem::size_of::<MemoryV2>() as c_uint) | (2 << 24);

#[repr(C)]
struct PciInfo {
    bus_id_legacy: [c_char; 16],
    domain: c_uint,
    bus: c_uint,
    device: c_uint,
    pci_device_id: c_uint,
    pci_sub_system_id: c_uint,
    bus_id: [c_char; 32],
}

struct Api {
    _lib: Library,
    init: unsafe extern "C" fn() -> Ret,
    shutdown: unsafe extern "C" fn() -> Ret,
    count: unsafe extern "C" fn(*mut c_uint) -> Ret,
    handle: unsafe extern "C" fn(c_uint, *mut Dev) -> Ret,
    name: unsafe extern "C" fn(Dev, *mut c_char, c_uint) -> Ret,
    pci: unsafe extern "C" fn(Dev, *mut PciInfo) -> Ret,
    util: unsafe extern "C" fn(Dev, *mut Utilization) -> Ret,
    temp: unsafe extern "C" fn(Dev, c_uint, *mut c_uint) -> Ret,
    clock: unsafe extern "C" fn(Dev, c_uint, *mut c_uint) -> Ret,
    power: unsafe extern "C" fn(Dev, *mut c_uint) -> Ret,
    mem_v1: unsafe extern "C" fn(Dev, *mut MemoryV1) -> Ret,
    mem_v2: Option<unsafe extern "C" fn(Dev, *mut MemoryV2) -> Ret>,
    error_string: unsafe extern "C" fn(Ret) -> *const c_char,
}

fn library_candidates() -> Vec<std::path::PathBuf> {
    #[cfg(windows)]
    {
        // Load by absolute path from System32 so a planted nvml.dll elsewhere on the DLL
        // search path can never be picked up.
        let sys = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
        vec![sys.join("System32").join("nvml.dll"), std::path::PathBuf::from(r"C:\Program Files\NVIDIA Corporation\NVSMI\nvml.dll")]
    }
    #[cfg(not(windows))]
    {
        vec!["libnvidia-ml.so.1".into(), "libnvidia-ml.so".into()]
    }
}

impl Api {
    fn load() -> Result<Api, String> {
        let mut last = String::from("NVIDIA driver (NVML) not installed");
        for path in library_candidates() {
            #[cfg(windows)]
            if !path.exists() {
                continue;
            }
            // SAFETY: loading the vendor's own library; its initialisers are trusted.
            let lib = match unsafe { Library::new(&path) } {
                Ok(l) => l,
                Err(e) => {
                    last = format!("could not load {}: {e}", path.display());
                    continue;
                }
            };
            macro_rules! sym {
                ($name:literal) => {
                    *unsafe { lib.get($name) }.map_err(|e| format!("NVML symbol {} missing: {e}", String::from_utf8_lossy($name)))?
                };
            }
            let mem_v2 = unsafe { lib.get::<unsafe extern "C" fn(Dev, *mut MemoryV2) -> Ret>(b"nvmlDeviceGetMemoryInfo_v2") }.ok().map(|s| *s);
            let api = Api {
                init: sym!(b"nvmlInit_v2"),
                shutdown: sym!(b"nvmlShutdown"),
                count: sym!(b"nvmlDeviceGetCount_v2"),
                handle: sym!(b"nvmlDeviceGetHandleByIndex_v2"),
                name: sym!(b"nvmlDeviceGetName"),
                pci: sym!(b"nvmlDeviceGetPciInfo_v3"),
                util: sym!(b"nvmlDeviceGetUtilizationRates"),
                temp: sym!(b"nvmlDeviceGetTemperature"),
                clock: sym!(b"nvmlDeviceGetClockInfo"),
                power: sym!(b"nvmlDeviceGetPowerUsage"),
                mem_v1: sym!(b"nvmlDeviceGetMemoryInfo"),
                mem_v2,
                error_string: sym!(b"nvmlErrorString"),
                _lib: lib,
            };
            return Ok(api);
        }
        Err(last)
    }

    fn err(&self, r: Ret) -> String {
        match r {
            NVML_ERROR_NOT_SUPPORTED => "not supported by this GPU/driver".into(),
            NVML_ERROR_NO_PERMISSION => "permission denied by the NVIDIA driver".into(),
            _ => {
                let p = unsafe { (self.error_string)(r) };
                if p.is_null() { format!("NVML error {r}") } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
            }
        }
    }
}

struct Gpu {
    handle: Dev,
    id: String,
}

pub struct NvmlProvider {
    api: Option<Api>,
    gpus: Vec<Gpu>,
}

// NVML handles are process-global and documented as thread-safe; the provider is only
// used from the telemetry thread anyway.
unsafe impl Send for NvmlProvider {}

impl NvmlProvider {
    pub fn new() -> NvmlProvider {
        NvmlProvider { api: None, gpus: Vec::new() }
    }

    fn shutdown(&mut self) {
        if let Some(api) = self.api.take() {
            unsafe { (api.shutdown)() };
        }
        self.gpus.clear();
    }
}

/// Stable id shared with other GPU providers so their data merges onto one device.
pub fn pci_device_id(bus: u32, device: u32, function: u32) -> String {
    format!("gpu:pci:{bus:02x}:{device:02x}.{function:x}")
}

fn parse_bus_id(s: &str) -> Option<(u32, u32, u32)> {
    // "00000000:01:00.0"
    let mut parts = s.rsplitn(3, ':');
    let df = parts.next()?;
    let bus = u32::from_str_radix(parts.next()?, 16).ok()?;
    let (d, f) = df.split_once('.')?;
    Some((bus, u32::from_str_radix(d, 16).ok()?, u32::from_str_radix(f, 16).ok()?))
}

impl Provider for NvmlProvider {
    fn name(&self) -> &'static str {
        "NVML"
    }

    fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
        self.shutdown();
        let api = Api::load()?;
        let r = unsafe { (api.init)() };
        if r != NVML_SUCCESS {
            return Err(match r {
                NVML_ERROR_DRIVER_NOT_LOADED => "NVIDIA driver not loaded".into(),
                _ => format!("nvmlInit failed: {}", api.err(r)),
            });
        }
        let mut n: c_uint = 0;
        let r = unsafe { (api.count)(&mut n) };
        if r != NVML_SUCCESS {
            let e = api.err(r);
            unsafe { (api.shutdown)() };
            return Err(format!("nvmlDeviceGetCount failed: {e}"));
        }
        let mut devices = Vec::new();
        for i in 0..n {
            let mut h: Dev = std::ptr::null_mut();
            if unsafe { (api.handle)(i, &mut h) } != NVML_SUCCESS {
                continue;
            }
            let mut name = [0 as c_char; 96];
            let name = if unsafe { (api.name)(h, name.as_mut_ptr(), name.len() as c_uint) } == NVML_SUCCESS {
                unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned()
            } else {
                format!("NVIDIA GPU {i}")
            };
            let mut pci: PciInfo = unsafe { std::mem::zeroed() };
            let id = if unsafe { (api.pci)(h, &mut pci) } == NVML_SUCCESS {
                let bus_id = unsafe { CStr::from_ptr(pci.bus_id.as_ptr()) }.to_string_lossy().into_owned();
                parse_bus_id(&bus_id).map(|(b, d, f)| pci_device_id(b, d, f)).unwrap_or_else(|| format!("gpu:nvml:{i}"))
            } else {
                format!("gpu:nvml:{i}")
            };
            devices.push(DeviceInfo { id: id.clone(), kind: DeviceKind::Gpu, name, vendor: Vendor::Nvidia, integrated: false, provider: "NVML".into() });
            self.gpus.push(Gpu { handle: h, id });
        }
        self.api = Some(api);
        if devices.is_empty() {
            return Err("no NVIDIA GPUs found".into());
        }
        Ok(devices)
    }

    fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String> {
        let api = self.api.as_ref().ok_or("not initialised")?;
        let m = &req.metrics;
        let mut lost = false;
        for g in &self.gpus {
            let h = g.handle;
            let mut push = |metric: MetricId, r: Ret, v: f64| {
                if r == NVML_SUCCESS {
                    out.push(Sample::value(&g.id, metric, v));
                } else {
                    if r == NVML_ERROR_GPU_IS_LOST {
                        lost = true;
                    }
                    out.push(Sample::unavailable(&g.id, metric, api.err(r)));
                }
            };
            if m.contains(MetricId::GpuUsage) {
                let mut u = Utilization::default();
                let r = unsafe { (api.util)(h, &mut u) };
                push(MetricId::GpuUsage, r, u.gpu as f64);
            }
            if m.contains(MetricId::GpuTemp) {
                let mut t: c_uint = 0;
                let r = unsafe { (api.temp)(h, NVML_TEMPERATURE_GPU, &mut t) };
                push(MetricId::GpuTemp, r, t as f64);
            }
            if m.contains(MetricId::GpuClock) {
                let mut c: c_uint = 0;
                let r = unsafe { (api.clock)(h, NVML_CLOCK_GRAPHICS, &mut c) };
                push(MetricId::GpuClock, r, c as f64);
            }
            if m.contains(MetricId::GpuPower) {
                let mut mw: c_uint = 0;
                let r = unsafe { (api.power)(h, &mut mw) };
                push(MetricId::GpuPower, r, mw as f64 / 1000.0);
            }
            if m.any_of(&[MetricId::VramUsed, MetricId::VramUsage, MetricId::VramTotal]) {
                let (r, used, total) = match api.mem_v2 {
                    Some(f) => {
                        let mut mem = MemoryV2 { version: NVML_MEMORY_V2, ..Default::default() };
                        let r = unsafe { f(h, &mut mem) };
                        (r, mem.used as f64, mem.total as f64)
                    }
                    None => (NVML_ERROR_FUNCTION_NOT_FOUND, 0.0, 0.0),
                };
                let (r, used, total) = if r == NVML_SUCCESS {
                    (r, used, total)
                } else {
                    let mut mem = MemoryV1::default();
                    let r = unsafe { (api.mem_v1)(h, &mut mem) };
                    (r, mem.used as f64, mem.total as f64)
                };
                push(MetricId::VramUsed, r, used);
                push(MetricId::VramTotal, r, total);
                if r == NVML_SUCCESS && total > 0.0 {
                    push(MetricId::VramUsage, r, used / total * 100.0);
                } else {
                    push(MetricId::VramUsage, if r == NVML_SUCCESS { NVML_ERROR_NOT_SUPPORTED } else { r }, 0.0);
                }
            }
        }
        if lost {
            // Driver reset / GPU removed: fail so the engine re-initialises NVML.
            return Err("GPU is lost (driver reset or device removed)".into());
        }
        Ok(())
    }

    fn on_resume(&mut self) {
        // Handles can become invalid across sleep; force a clean re-init on next failure.
    }
}

impl Drop for NvmlProvider {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_id_parsing() {
        assert_eq!(parse_bus_id("00000000:01:00.0"), Some((1, 0, 0)));
        assert_eq!(parse_bus_id("0000:0a:1f.3"), Some((0x0a, 0x1f, 3)));
        assert_eq!(parse_bus_id("garbage"), None);
        assert_eq!(pci_device_id(1, 0, 0), "gpu:pci:01:00.0");
    }

    #[test]
    fn memory_v2_version_constant() {
        // NVML_STRUCT_VERSION(Memory, 2) = sizeof(nvmlMemory_v2_t) | 2 << 24 = 40 | 0x2000000
        assert_eq!(NVML_MEMORY_V2, 40 | (2 << 24));
    }
}
