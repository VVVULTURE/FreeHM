//! The helper's named-pipe server.
//!
//! Each request is the fixed [`HelperRequest`] (two booleans, size-limited, unknown fields
//! rejected); the response is the current snapshot. Requests are rate-limited per
//! connection and the number of simultaneous connections is capped by the pipe's
//! instance limit.

use super::cpu::CpuReader;
use super::etw::FrameCollector;
use super::sd::{HELPER_PIPE_SDDL, SecurityDescriptor};
use freehm_core::helper_proto::{HelperRequest, HelperResponse, MAX_REQUEST, MAX_RESPONSE, PIPE_NAME, PROTOCOL_VERSION};
use freehm_core::wire::{read_frame, write_frame};
use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows::core::PCWSTR;

const MAX_INSTANCES: u32 = 4;
/// Minimum spacing of requests on one connection (the app asks every 200 ms by default).
const MIN_REQUEST_SPACING: Duration = Duration::from_millis(20);

pub struct Sensors {
    cpu: CpuReader,
    frames: FrameCollector,
}

impl Sensors {
    pub fn new() -> Sensors {
        Sensors { cpu: CpuReader::new(), frames: FrameCollector::new() }
    }

    pub fn answer(&mut self, req: &HelperRequest) -> HelperResponse {
        HelperResponse {
            v: PROTOCOL_VERSION,
            helper_version: freehm_core::APP_VERSION.into(),
            error: None,
            cpu: req.cpu.then(|| self.cpu.read()),
            frames: req.frames.then(|| self.frames.report()),
        }
    }

    pub fn housekeeping(&mut self) {
        self.cpu.release_if_idle();
        self.frames.stop_if_idle();
    }

    pub fn shutdown(&mut self) {
        self.frames.stop();
        self.cpu.release();
    }
}

fn create_instance(name: &[u16], sd: &SecurityDescriptor, first: bool) -> Result<File, String> {
    let sa = sd.attributes();
    let mut mode = PIPE_ACCESS_DUPLEX;
    if first {
        mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            MAX_INSTANCES,
            64 * 1024,
            4096,
            0,
            Some(&sa),
        )
    };
    if h.is_invalid() {
        return Err(format!("CreateNamedPipe: {}", windows::core::Error::from_thread()));
    }
    Ok(unsafe { File::from_raw_handle(h.0) })
}

fn serve_client(mut pipe: File, sensors: Arc<Mutex<Sensors>>, stop: Arc<AtomicBool>, active: Arc<std::sync::atomic::AtomicU32>) {
    let mut last = Instant::now() - MIN_REQUEST_SPACING;
    while !stop.load(Ordering::Relaxed) {
        let req: HelperRequest = match read_frame(&mut pipe, MAX_REQUEST) {
            Ok(r) => r,
            Err(_) => break, // disconnect, oversized or malformed request: drop the client
        };
        let since = last.elapsed();
        if since < MIN_REQUEST_SPACING {
            std::thread::sleep(MIN_REQUEST_SPACING - since);
        }
        last = Instant::now();
        let resp = match req.validate() {
            Ok(()) => sensors.lock().map(|mut s| s.answer(&req)).unwrap_or_default(),
            Err(e) => HelperResponse { v: PROTOCOL_VERSION, helper_version: freehm_core::APP_VERSION.into(), error: Some(e), ..Default::default() },
        };
        if write_frame(&mut pipe, &resp, MAX_RESPONSE).is_err() {
            break;
        }
    }
    active.fetch_sub(1, Ordering::Relaxed);
}

pub struct Server {
    stop: Arc<AtomicBool>,
    sensors: Arc<Mutex<Sensors>>,
    listener: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn start() -> Result<Server, String> {
        let name: Vec<u16> = PIPE_NAME.encode_utf16().chain(std::iter::once(0)).collect();
        let sd = SecurityDescriptor::from_sddl(HELPER_PIPE_SDDL)?;
        // Fails if any other process already owns the name.
        let first = create_instance(&name, &sd, true).map_err(|e| format!("cannot create the helper pipe (is another copy running?): {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let sensors = Arc::new(Mutex::new(Sensors::new()));
        {
            let s = sensors.lock().unwrap();
            log::info!("CPU: {} — {:?}", s.cpu.name, s.cpu.kind);
        }
        let (stop2, sensors2) = (stop.clone(), sensors.clone());
        let listener = std::thread::Builder::new()
            .name("pipe-listen".into())
            .spawn(move || {
                let active = Arc::new(std::sync::atomic::AtomicU32::new(0));
                let mut next = Some(first);
                while !stop2.load(Ordering::Relaxed) {
                    let pipe = match next.take() {
                        Some(p) => p,
                        None => match create_instance(&name, &sd, false) {
                            Ok(p) => p,
                            Err(e) => {
                                // All instances busy, or a transient failure.
                                log::debug!("{e}");
                                std::thread::sleep(Duration::from_millis(500));
                                continue;
                            }
                        },
                    };
                    let h = HANDLE(pipe.as_raw_handle());
                    let connected = match unsafe { ConnectNamedPipe(h, None) } {
                        Ok(()) => true,
                        Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
                    };
                    if !connected || stop2.load(Ordering::Relaxed) {
                        continue;
                    }
                    active.fetch_add(1, Ordering::Relaxed);
                    let (s, st, a) = (sensors2.clone(), stop2.clone(), active.clone());
                    if std::thread::Builder::new().name("pipe-client".into()).spawn(move || serve_client(pipe, s, st, a)).is_err() {
                        active.fetch_sub(1, Ordering::Relaxed);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        // Housekeeping: unload the PawnIO module / stop ETW when idle.
        {
            let (stop3, sensors3) = (stop.clone(), sensors.clone());
            let _ = std::thread::Builder::new().name("housekeeping".into()).spawn(move || {
                while !stop3.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                    if let Ok(mut s) = sensors3.lock() {
                        s.housekeeping();
                    }
                }
            });
        }
        log::info!("helper listening on {PIPE_NAME}");
        Ok(Server { stop, sensors, listener: Some(listener) })
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the listener blocked in ConnectNamedPipe by connecting to it once.
        let _ = std::fs::OpenOptions::new().read(true).open(PIPE_NAME);
        if let Some(t) = self.listener.take() {
            let _ = t.join();
        }
        if let Ok(mut s) = self.sensors.lock() {
            s.shutdown();
        }
        log::info!("helper stopped");
    }
}

/// `freehm-helper selftest`: read everything once without installing anything (needs an
/// elevated console). Useful for checking a machine before installing the service.
pub fn selftest() -> i32 {
    let mut s = Sensors::new();
    println!("CPU: {} — {:?}", s.cpu.name, s.cpu.kind);
    let req = HelperRequest { v: PROTOCOL_VERSION, cpu: true, frames: true };
    let first = s.answer(&req);
    std::thread::sleep(Duration::from_millis(1000));
    let r = s.answer(&req);
    println!("cpu (first read): {:?}", first.cpu);
    println!("cpu: {:?}", r.cpu);
    println!("sampling frames for 3 s…");
    std::thread::sleep(Duration::from_secs(3));
    let r = s.answer(&req);
    match r.frames {
        Some(f) if f.available => {
            println!("frame timing: {} presenting process(es)", f.processes.len());
            for p in f.processes.iter().take(10) {
                println!("  pid {:>6}  {:?}  {:.1} fps  {:.2} ms", p.pid, p.source, p.stats.fps, p.stats.frametime_ms);
            }
        }
        Some(f) => println!("frame timing unavailable: {}", f.reason.unwrap_or_default()),
        None => {}
    }
    s.shutdown();
    0
}
