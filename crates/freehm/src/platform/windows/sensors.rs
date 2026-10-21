//! Unprivileged Windows telemetry providers.
//!
//! - CPU: PDH `Processor Information` (the same counters Task Manager uses) and the
//!   `Energy Meter` RAPL counters exposed by Windows' Energy Metering Interface.
//! - Memory: `GlobalMemoryStatusEx`.
//! - GPU (any vendor): WDDM kernel-thunk queries (`D3DKMTQueryStatistics` for engine
//!   running time, `D3DKMTQueryAdapterInfo` for clock/temperature) plus PDH
//!   `GPU Adapter Memory` for dedicated/shared memory in use.
//!
//! CPU temperature has no documented unprivileged interface on Windows; it comes from the
//! helper service (see `helper_client`).

use super::pdh::{self, Counter, Query, Read};
use super::util::{from_wide, reg_string};
use crate::sensors::nvml::pci_device_id;
use freehm_core::metrics::{DEVICE_MEMORY, DeviceInfo, DeviceKind, MetricId, Vendor};
use freehm_core::telemetry::{PollRequest, Provider, Sample};
use std::time::Instant;
use windows::Wdk::Graphics::Direct3D::*;
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

pub const CPU_DEVICE: &str = "cpu:0";

pub fn cpu_name() -> String {
    reg_string(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "CPU".into())
}

pub fn cpu_vendor() -> Vendor {
    match reg_string(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "VendorIdentifier").as_deref() {
        Some("GenuineIntel") => Vendor::Intel,
        Some("AuthenticAMD") => Vendor::Amd,
        _ => Vendor::Other,
    }
}

pub struct CpuProvider {
    q: Option<Query>,
    usage: Option<Counter>,
    freq: Option<Counter>,
    perf: Option<Counter>,
    power: Result<Counter, String>,
}

impl CpuProvider {
    pub fn new() -> CpuProvider {
        CpuProvider { q: None, usage: None, freq: None, perf: None, power: Err("not initialised".into()) }
    }
}

impl Provider for CpuProvider {
    fn name(&self) -> &'static str {
        "Windows PDH (CPU)"
    }

    fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
        let mut q = Query::open()?;
        // "% Processor Utility" matches Task Manager on Windows 8+; older systems lack it.
        self.usage = q.add(r"\Processor Information(_Total)\% Processor Utility").or_else(|_| q.add(r"\Processor(_Total)\% Processor Time")).ok();
        self.freq = q.add(r"\Processor Information(_Total)\Processor Frequency").ok();
        self.perf = q.add(r"\Processor Information(_Total)\% Processor Performance").ok();
        self.power = q
            .add(r"\Energy Meter(RAPL_Package0_PKG)\Power")
            .map_err(|e| format!("Windows Energy Meter (RAPL) is not available on this system ({}); the FreeHM helper can read it instead", pdh::describe(e)));
        q.collect().map_err(|e| format!("PDH collect failed: {}", pdh::describe(e)))?;
        self.q = Some(q);
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
        if !m.any_of(&[MetricId::CpuUsage, MetricId::CpuClock, MetricId::CpuPower]) {
            return Ok(());
        }
        let q = self.q.as_mut().ok_or("not initialised")?;
        q.collect().map_err(|e| format!("PDH collect failed: {}", pdh::describe(e)))?;
        let q = &*q;
        let read = |c: Option<Counter>| c.map(|c| q.read(c));
        if m.contains(MetricId::CpuUsage) {
            match read(self.usage) {
                Some(Read::Value(v)) => out.push(Sample::value(CPU_DEVICE, MetricId::CpuUsage, v.min(100.0))),
                Some(Read::NotReady) => {}
                Some(Read::Error(e)) => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuUsage, pdh::describe(e))),
                None => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuUsage, "processor utilisation counter missing")),
            }
        }
        if m.contains(MetricId::CpuClock) {
            // Effective clock = base frequency × relative performance (Task Manager method).
            match (read(self.freq), read(self.perf)) {
                (Some(Read::Value(f)), Some(Read::Value(p))) => out.push(Sample::value(CPU_DEVICE, MetricId::CpuClock, f * p / 100.0)),
                (Some(Read::NotReady), _) | (_, Some(Read::NotReady)) => {}
                _ => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuClock, "processor frequency counters not available")),
            }
        }
        if m.contains(MetricId::CpuPower) {
            match &self.power {
                Ok(c) => match q.read(*c) {
                    // The Energy Meter reports milliwatts.
                    Read::Value(mw) => out.push(Sample::value(CPU_DEVICE, MetricId::CpuPower, mw / 1000.0)),
                    Read::NotReady => {}
                    Read::Error(e) => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuPower, pdh::describe(e))),
                },
                Err(reason) => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuPower, reason.clone())),
            }
        }
        Ok(())
    }
}

pub struct MemoryProvider;

impl Provider for MemoryProvider {
    fn name(&self) -> &'static str {
        "Windows memory status"
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
        let mut ms = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        unsafe { GlobalMemoryStatusEx(&mut ms) }.map_err(|e| format!("GlobalMemoryStatusEx failed: {e}"))?;
        let total = ms.ullTotalPhys as f64;
        let used = total - ms.ullAvailPhys as f64;
        out.push(Sample::value(DEVICE_MEMORY, MetricId::RamTotal, total));
        out.push(Sample::value(DEVICE_MEMORY, MetricId::RamUsed, used));
        out.push(Sample::value(DEVICE_MEMORY, MetricId::RamUsage, used / total * 100.0));
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// WDDM GPU provider

const ADAPTERTYPE_RENDER: u32 = 1 << 0;
const ADAPTERTYPE_SOFTWARE: u32 = 1 << 2;
const ADAPTERTYPE_HYBRID_INTEGRATED: u32 = 1 << 5;
const MIB: u64 = 1024 * 1024;

struct Adapter {
    handle: u32,
    luid: LUID,
    id: String,
    vendor: Vendor,
    integrated: bool,
    dedicated_total: u64,
    node_count: u32,
    /// Previous (running time per node in 100 ns, wall clock) for utilisation deltas.
    prev: Option<(Vec<i64>, Instant)>,
}

pub struct WddmGpuProvider {
    adapters: Vec<Adapter>,
    pdh: Option<(Query, Option<Counter>, Option<Counter>)>,
}

impl WddmGpuProvider {
    pub fn new() -> WddmGpuProvider {
        WddmGpuProvider { adapters: Vec::new(), pdh: None }
    }

    fn close(&mut self) {
        for a in self.adapters.drain(..) {
            let c = D3DKMT_CLOSEADAPTER { hAdapter: a.handle };
            unsafe {
                let _ = D3DKMTCloseAdapter(&c);
            }
        }
    }
}

impl Drop for WddmGpuProvider {
    fn drop(&mut self) {
        self.close();
    }
}

fn query_info<T: Default>(handle: u32, ty: KMTQUERYADAPTERINFOTYPE, mut data: T) -> Option<T> {
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: ty,
        pPrivateDriverData: (&mut data as *mut T).cast(),
        PrivateDriverDataSize: std::mem::size_of::<T>() as u32,
    };
    let st = unsafe { D3DKMTQueryAdapterInfo(&mut q) };
    st.is_ok().then_some(data)
}

fn query_stats(luid: LUID, ty: D3DKMT_QUERYSTATISTICS_TYPE, set: impl FnOnce(&mut D3DKMT_QUERYSTATISTICS)) -> Option<Box<D3DKMT_QUERYSTATISTICS>> {
    // The structure is large (the result union is several KB); keep it off the stack.
    let mut q: Box<D3DKMT_QUERYSTATISTICS> = Box::new(unsafe { std::mem::zeroed() });
    q.Type = ty;
    q.AdapterLuid = luid;
    set(&mut q);
    let st = unsafe { D3DKMTQueryStatistics(&*q) };
    st.is_ok().then_some(q)
}

fn adapter_names() -> Vec<(LUID, String, u64)> {
    let mut out = Vec::new();
    let Ok(f) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return out };
    for i in 0.. {
        let Ok(a) = (unsafe { f.EnumAdapters1(i) }) else { break };
        if let Ok(d) = unsafe { a.GetDesc1() } {
            out.push((d.AdapterLuid, from_wide(&d.Description), d.DedicatedVideoMemory as u64));
        }
    }
    out
}

fn luid_instance(luid: LUID) -> String {
    format!("luid_0x{:08x}_0x{:08x}", luid.HighPart as u32, luid.LowPart)
}

impl Provider for WddmGpuProvider {
    fn name(&self) -> &'static str {
        "Windows WDDM (GPU)"
    }

    fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
        self.close();
        let mut e = D3DKMT_ENUMADAPTERS2 { NumAdapters: 0, pAdapters: std::ptr::null_mut() };
        unsafe { D3DKMTEnumAdapters2(&mut e) }.ok().map_err(|err| format!("D3DKMTEnumAdapters2 failed: {err}"))?;
        let mut infos = vec![D3DKMT_ADAPTERINFO::default(); e.NumAdapters as usize];
        e.pAdapters = infos.as_mut_ptr();
        unsafe { D3DKMTEnumAdapters2(&mut e) }.ok().map_err(|err| format!("D3DKMTEnumAdapters2 failed: {err}"))?;
        infos.truncate(e.NumAdapters as usize);
        let names = adapter_names();
        let mut devices = Vec::new();
        for info in infos {
            let close = || unsafe {
                let _ = D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: info.hAdapter });
            };
            let ty = query_info(info.hAdapter, KMTQAITYPE_ADAPTERTYPE, D3DKMT_ADAPTERTYPE::default()).map(|t| unsafe { t.Anonymous.Value }).unwrap_or(0);
            // Skip the Microsoft Basic Render Driver and display-only adapters (indirect
            // display drivers, virtual displays): they have no GPU to monitor.
            if ty & ADAPTERTYPE_RENDER == 0 || ty & ADAPTERTYPE_SOFTWARE != 0 {
                close();
                continue;
            }
            let ids = query_info(info.hAdapter, KMTQAITYPE_PHYSICALADAPTERDEVICEIDS, D3DKMT_QUERY_DEVICE_IDS::default());
            let vendor = ids.as_ref().map(|i| Vendor::from_pci_id(i.DeviceIds.VendorID)).unwrap_or(Vendor::Other);
            let id = match query_info(info.hAdapter, KMTQAITYPE_ADAPTERADDRESS, D3DKMT_ADAPTERADDRESS::default()) {
                Some(a) => pci_device_id(a.BusNumber, a.DeviceNumber, a.FunctionNumber),
                None => format!("gpu:luid:{:x}:{:x}", info.AdapterLuid.HighPart, info.AdapterLuid.LowPart),
            };
            let seg = query_info(info.hAdapter, KMTQAITYPE_GETSEGMENTSIZE, D3DKMT_SEGMENTSIZEINFO::default()).unwrap_or_default();
            let (name, dxgi_dedicated) = names
                .iter()
                .find(|(l, _, _)| l.LowPart == info.AdapterLuid.LowPart && l.HighPart == info.AdapterLuid.HighPart)
                .map(|(_, n, d)| (n.clone(), *d))
                .unwrap_or_else(|| ("GPU".into(), 0));
            let dedicated_total = if seg.DedicatedVideoMemorySize > 0 { seg.DedicatedVideoMemorySize } else { dxgi_dedicated };
            // Integrated GPUs only have a small firmware carve-out ("dedicated" in name only)
            // and allocate from shared system memory.
            let integrated = ty & ADAPTERTYPE_HYBRID_INTEGRATED != 0 || (matches!(vendor, Vendor::Intel | Vendor::Amd) && dedicated_total <= 512 * MIB);
            let node_count = query_stats(info.AdapterLuid, D3DKMT_QUERYSTATISTICS_ADAPTER, |_| {})
                .map(|q| unsafe { q.QueryResult.AdapterInformation.NodeCount })
                .unwrap_or(0);
            devices.push(DeviceInfo { id: id.clone(), kind: DeviceKind::Gpu, name, vendor, integrated, provider: self.name().into() });
            self.adapters.push(Adapter { handle: info.hAdapter, luid: info.AdapterLuid, id, vendor, integrated, dedicated_total, node_count, prev: None });
        }
        // Memory usage per adapter (instances are named by adapter LUID).
        self.pdh = Query::open().ok().map(|mut q| {
            let d = q.add(r"\GPU Adapter Memory(*)\Dedicated Usage").ok();
            let s = q.add(r"\GPU Adapter Memory(*)\Shared Usage").ok();
            (q, d, s)
        });
        if devices.is_empty() {
            return Err("no hardware GPU adapters found".into());
        }
        Ok(devices)
    }

    fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String> {
        let m = req.metrics;
        let want_mem = m.any_of(&[MetricId::VramUsed, MetricId::VramUsage, MetricId::VramTotal, MetricId::GpuSharedMem]);
        let (mut dedicated, mut shared) = (Vec::new(), Vec::new());
        if want_mem
            && let Some((q, d, s)) = self.pdh.as_mut()
            && q.collect().is_ok()
        {
            if let Some(d) = d {
                dedicated = q.read_array(*d).unwrap_or_default();
            }
            if let Some(s) = s {
                shared = q.read_array(*s).unwrap_or_default();
            }
        }
        let now = Instant::now();
        for a in &mut self.adapters {
            if m.contains(MetricId::GpuUsage) && a.node_count > 0 {
                let mut times = Vec::with_capacity(a.node_count as usize);
                for node in 0..a.node_count {
                    let t = query_stats(a.luid, D3DKMT_QUERYSTATISTICS_NODE, |q| q.Anonymous.QueryNode.NodeId = node)
                        .map(|q| unsafe { q.QueryResult.NodeInformation.GlobalInformation.RunningTime })
                        .unwrap_or(0);
                    times.push(t);
                }
                if let Some((prev, at)) = &a.prev {
                    let wall = now.duration_since(*at).as_nanos() as f64 / 100.0; // 100 ns units
                    if wall > 0.0 {
                        // Task Manager's "GPU" figure: the busiest engine.
                        let busiest = times.iter().zip(prev).map(|(t, p)| (t - p).max(0) as f64 / wall * 100.0).fold(0.0, f64::max);
                        out.push(Sample::value(&a.id, MetricId::GpuUsage, busiest.min(100.0)));
                    }
                }
                a.prev = Some((times, now));
            } else if m.contains(MetricId::GpuUsage) {
                out.push(Sample::unavailable(&a.id, MetricId::GpuUsage, "driver reports no GPU engines"));
            }
            if m.contains(MetricId::GpuClock) {
                let pd = query_info(a.handle, KMTQAITYPE_NODEPERFDATA, D3DKMT_NODE_PERFDATA { NodeOrdinal: 0, PhysicalAdapterIndex: 0, ..Default::default() });
                match pd {
                    Some(p) if p.Frequency > 0 => out.push(Sample::value(&a.id, MetricId::GpuClock, p.Frequency as f64 / 1e6)),
                    _ => out.push(Sample::unavailable(&a.id, MetricId::GpuClock, "the graphics driver does not report engine frequency to Windows")),
                }
            }
            if m.contains(MetricId::GpuTemp) {
                let pd = query_info(a.handle, KMTQAITYPE_ADAPTERPERFDATA, D3DKMT_ADAPTER_PERFDATA { PhysicalAdapterIndex: 0, ..Default::default() });
                match pd {
                    // Deci-degrees Celsius; 0 means "not reported".
                    Some(p) if p.Temperature > 0 => out.push(Sample::value(&a.id, MetricId::GpuTemp, p.Temperature as f64 / 10.0)),
                    _ => out.push(Sample::unavailable(&a.id, MetricId::GpuTemp, "the graphics driver does not report temperature to Windows")),
                }
            }
            if m.contains(MetricId::GpuPower) {
                out.push(Sample::unavailable(
                    &a.id,
                    MetricId::GpuPower,
                    match a.vendor {
                        Vendor::Nvidia => "NVML did not provide power for this GPU",
                        _ => "Windows reports GPU power only as a percentage of TDP; watts need a vendor API that FreeHM does not use for this GPU",
                    },
                ));
            }
            if want_mem {
                let inst = luid_instance(a.luid);
                let sum = |v: &[(String, f64)]| -> Option<f64> {
                    let matching: Vec<f64> = v.iter().filter(|(n, _)| n.to_ascii_lowercase().starts_with(&inst)).map(|(_, x)| *x).collect();
                    (!matching.is_empty()).then(|| matching.iter().sum())
                };
                match sum(&shared) {
                    Some(s) => out.push(Sample::value(&a.id, MetricId::GpuSharedMem, s)),
                    None => out.push(Sample::unavailable(&a.id, MetricId::GpuSharedMem, "GPU Adapter Memory counters not available")),
                }
                if a.integrated {
                    let why = "integrated GPU: no dedicated VRAM (it uses shared system memory — see GPU shared memory)";
                    for mm in [MetricId::VramUsed, MetricId::VramUsage, MetricId::VramTotal] {
                        out.push(Sample::unavailable(&a.id, mm, why));
                    }
                } else {
                    let total = a.dedicated_total as f64;
                    if total > 0.0 {
                        out.push(Sample::value(&a.id, MetricId::VramTotal, total));
                    }
                    match sum(&dedicated) {
                        Some(u) => {
                            out.push(Sample::value(&a.id, MetricId::VramUsed, u));
                            if total > 0.0 {
                                out.push(Sample::value(&a.id, MetricId::VramUsage, u / total * 100.0));
                            }
                        }
                        None => {
                            out.push(Sample::unavailable(&a.id, MetricId::VramUsed, "GPU Adapter Memory counters not available"));
                            out.push(Sample::unavailable(&a.id, MetricId::VramUsage, "GPU Adapter Memory counters not available"));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn on_resume(&mut self) {
        for a in &mut self.adapters {
            a.prev = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luid_instance_format_matches_pdh() {
        let l = LUID { LowPart: 0xD2B5, HighPart: 0 };
        assert_eq!(luid_instance(l), "luid_0x00000000_0x0000d2b5");
    }
}
