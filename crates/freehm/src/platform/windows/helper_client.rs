//! Client for the FreeHM helper service (CPU temperature via PawnIO, frame statistics via
//! ETW). Pipe I/O happens on a dedicated worker thread so a slow or stuck helper can never
//! block the telemetry thread; the provider just reads the latest response.
//!
//! Before trusting a connection, the client checks that the pipe's server process is the
//! process the Service Control Manager reports for the FreeHM helper service, so another
//! program squatting on the pipe name cannot feed it fabricated readings.

use super::util::{reg_key_exists, wide};
use freehm_core::helper_proto::{HelperRequest, HelperResponse, MAX_REQUEST, MAX_RESPONSE, PIPE_NAME, PROTOCOL_VERSION, SERVICE_NAME, Value};
use freehm_core::metrics::{DEVICE_GAME, DeviceInfo, MetricId};
use freehm_core::telemetry::{PollRequest, Provider, Sample};
use freehm_core::wire::{read_frame, write_frame};
use std::fs::File;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::Win32::System::Services::*;
use windows::core::PCWSTR;

use super::sensors::CPU_DEVICE;

#[derive(Debug, Clone, Default)]
pub struct ServiceInfo {
    pub installed: bool,
    pub running: bool,
    pub pid: u32,
}

pub fn service_info() -> ServiceInfo {
    unsafe {
        let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else {
            return ServiceInfo::default();
        };
        let name = wide(SERVICE_NAME);
        let info = match OpenServiceW(scm, PCWSTR(name.as_ptr()), SERVICE_QUERY_STATUS) {
            Ok(svc) => {
                let mut st = SERVICE_STATUS_PROCESS::default();
                let mut needed = 0u32;
                let buf = std::slice::from_raw_parts_mut((&mut st as *mut SERVICE_STATUS_PROCESS).cast::<u8>(), std::mem::size_of::<SERVICE_STATUS_PROCESS>());
                let ok = QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, Some(buf), &mut needed).is_ok();
                let _ = CloseServiceHandle(svc);
                ServiceInfo { installed: true, running: ok && st.dwCurrentState == SERVICE_RUNNING, pid: if ok { st.dwProcessId } else { 0 } }
            }
            Err(_) => ServiceInfo::default(),
        };
        let _ = CloseServiceHandle(scm);
        info
    }
}

pub fn pawnio_installed() -> bool {
    reg_key_exists(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Services\PawnIO")
}

#[derive(Default)]
pub struct HelperShared {
    pub connected: bool,
    pub message: String,
    latest: Option<(Instant, HelperResponse)>,
}

pub struct HelperProvider {
    shared: Arc<Mutex<HelperShared>>,
    enabled: Arc<AtomicBool>,
    tx: Option<mpsc::Sender<HelperRequest>>,
}

impl HelperProvider {
    pub fn new(shared: Arc<Mutex<HelperShared>>, enabled: Arc<AtomicBool>) -> HelperProvider {
        HelperProvider { shared, enabled, tx: None }
    }
}

fn connect() -> Result<File, String> {
    let svc = service_info();
    if !svc.installed {
        return Err("the FreeHM helper service is not installed (Settings ▸ Startup & permissions)".into());
    }
    if !svc.running {
        return Err("the FreeHM helper service is installed but not running".into());
    }
    // Request only what the pipe's DACL grants interactive users (read/write data, read
    // attributes, synchronize); GENERIC_WRITE would include FILE_CREATE_PIPE_INSTANCE.
    use std::os::windows::fs::OpenOptionsExt;
    const ACCESS: u32 = 0x0001 | 0x0002 | 0x0080 | 0x0010_0000;
    let f = std::fs::OpenOptions::new().access_mode(ACCESS).open(PIPE_NAME).map_err(|e| format!("cannot connect to helper: {e}"))?;
    let mut server_pid = 0u32;
    unsafe { GetNamedPipeServerProcessId(HANDLE(f.as_raw_handle()), &mut server_pid) }.map_err(|e| format!("cannot verify helper: {e}"))?;
    if server_pid != svc.pid {
        return Err(format!("refusing helper pipe served by pid {server_pid}, expected service pid {}", svc.pid));
    }
    Ok(f)
}

fn worker(rx: mpsc::Receiver<HelperRequest>, shared: Arc<Mutex<HelperShared>>) {
    let mut conn: Option<File> = None;
    let mut retry_at = Instant::now();
    let mut last_err = String::new();
    loop {
        // Block until the provider asks for data; exit when the provider is dropped.
        let mut req = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => {
                conn = None; // idle: release the connection so the helper can stop ETW
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Coalesce queued requests into the newest one.
        while let Ok(r) = rx.try_recv() {
            req = r;
        }
        if conn.is_none() {
            if Instant::now() < retry_at {
                continue;
            }
            match connect() {
                Ok(f) => {
                    log::info!("connected to helper service");
                    conn = Some(f);
                    last_err.clear();
                }
                Err(e) => {
                    if e != last_err {
                        log::info!("helper unavailable: {e}");
                        last_err = e.clone();
                    }
                    let mut s = shared.lock().unwrap();
                    s.connected = false;
                    s.message = e;
                    retry_at = Instant::now() + Duration::from_secs(5);
                    continue;
                }
            }
        }
        let f = conn.as_mut().unwrap();
        let result = write_frame(f, &req, MAX_REQUEST).and_then(|_| read_frame::<HelperResponse, _>(f, MAX_RESPONSE));
        let mut s = shared.lock().unwrap();
        match result {
            Ok(resp) => {
                s.connected = true;
                s.message = match &resp.error {
                    Some(e) => e.clone(),
                    None => format!("connected (helper {})", resp.helper_version),
                };
                s.latest = Some((Instant::now(), resp));
            }
            Err(e) => {
                log::warn!("helper connection lost: {e}");
                s.connected = false;
                s.message = format!("connection lost: {e}");
                conn = None;
                retry_at = Instant::now() + Duration::from_secs(1);
            }
        }
    }
}

impl Provider for HelperProvider {
    fn name(&self) -> &'static str {
        "FreeHM helper service"
    }

    fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
        if self.tx.is_none() {
            let (tx, rx) = mpsc::channel();
            let shared = self.shared.clone();
            std::thread::Builder::new().name("helper-client".into()).spawn(move || worker(rx, shared)).map_err(|e| e.to_string())?;
            self.tx = Some(tx);
        }
        Ok(Vec::new())
    }

    fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String> {
        let m = req.metrics;
        let want_cpu = m.any_of(&[MetricId::CpuTemp, MetricId::CpuPower]);
        let want_frames = m.any_of(&[MetricId::Fps, MetricId::FrameTime]);
        if !want_cpu && !want_frames {
            return Ok(());
        }
        let unavailable_all = |out: &mut Vec<Sample>, why: &str| {
            if m.contains(MetricId::CpuTemp) {
                out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuTemp, why));
            }
            if m.contains(MetricId::CpuPower) {
                out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuPower, why));
            }
            for f in [MetricId::Fps, MetricId::FrameTime] {
                if m.contains(f) {
                    out.push(Sample::unavailable(DEVICE_GAME, f, why));
                }
            }
        };
        if !self.enabled.load(Ordering::Relaxed) {
            unavailable_all(out, "the helper service is disabled in settings");
            return Ok(());
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(HelperRequest { v: PROTOCOL_VERSION, cpu: want_cpu, frames: want_frames && !req.game_pids.is_empty() });
        }
        let s = self.shared.lock().unwrap();
        let latest = s.latest.as_ref().filter(|(t, _)| t.elapsed() < Duration::from_secs(2) && s.connected);
        let Some((_, resp)) = latest else {
            if !s.connected && !s.message.is_empty() {
                unavailable_all(out, &s.message);
            }
            // Otherwise the first response is still on its way; emit nothing this cycle.
            return Ok(());
        };
        if want_cpu {
            match &resp.cpu {
                Some(c) => {
                    match &c.package_temp_c {
                        Value::Ok(t) => out.push(Sample::value(CPU_DEVICE, MetricId::CpuTemp, *t)),
                        Value::Unavailable(r) => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuTemp, r.clone())),
                    }
                    match &c.package_power_w {
                        Value::Ok(w) => out.push(Sample::value(CPU_DEVICE, MetricId::CpuPower, *w)),
                        Value::Unavailable(r) => out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuPower, r.clone())),
                    }
                }
                None => {
                    let why = resp.error.clone().unwrap_or_else(|| "helper returned no CPU data".into());
                    out.push(Sample::unavailable(CPU_DEVICE, MetricId::CpuTemp, why));
                }
            }
        }
        if want_frames {
            let push_na = |out: &mut Vec<Sample>, why: &str| {
                for f in [MetricId::Fps, MetricId::FrameTime] {
                    out.push(Sample::unavailable(DEVICE_GAME, f, why));
                }
            };
            if req.game_pids.is_empty() {
                push_na(out, "no game detected");
            } else {
                match &resp.frames {
                    Some(fr) if fr.available => {
                        // A game may present from a child process; use whichever process in
                        // its tree presented the most frames.
                        let best = fr.processes.iter().filter(|p| req.game_pids.contains(&p.pid)).max_by_key(|p| p.stats.samples);
                        match best {
                            Some(p) => {
                                out.push(Sample::value(DEVICE_GAME, MetricId::Fps, p.stats.fps));
                                out.push(Sample::value(DEVICE_GAME, MetricId::FrameTime, p.stats.frametime_ms));
                            }
                            None => push_na(out, "the game has not presented any frames in the last two seconds"),
                        }
                    }
                    Some(fr) => push_na(out, fr.reason.as_deref().unwrap_or("frame statistics unavailable")),
                    None => push_na(out, "waiting for frame statistics"),
                }
            }
        }
        Ok(())
    }
}
