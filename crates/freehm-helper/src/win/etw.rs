//! Present timing from the operating system's graphics ETW providers — the same events
//! PresentMon uses. A private real-time session is started on demand and stopped after
//! [`IDLE_STOP`] without requests. Only event headers are read (process id, timestamp,
//! provider, event id); nothing touches the game process.

use freehm_core::frames::{PresentSource, PresentTracker};
use freehm_core::helper_proto::FrameReport;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Diagnostics::Etw::*;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::core::{GUID, PCWSTR, PWSTR};

pub const IDLE_STOP: Duration = Duration::from_secs(10);
const SESSION_NAME: &str = "FreeHM-Frames";

/// Microsoft-Windows-DXGI — Present_Start (0x2A).
const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
/// Microsoft-Windows-D3D9 — Present_Start (1).
const D3D9: GUID = GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
/// Microsoft-Windows-DxgKrnl — Present_Info (0xB8).
const DXGKRNL: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);

fn qpc_freq() -> u64 {
    static F: OnceLock<u64> = OnceLock::new();
    *F.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1) as u64
    })
}

fn qpc_to_ns(t: u64) -> u64 {
    (t as u128 * 1_000_000_000 / qpc_freq() as u128) as u64
}

fn now_ns() -> u64 {
    let mut t = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    qpc_to_ns(t as u64)
}

/// Event counters for `freehm-helper selftest`: all events, events per provider, accepted.
static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

pub fn debug_counts() -> String {
    let c: Vec<u64> = COUNTS.iter().map(|c| c.load(Ordering::Relaxed)).collect();
    format!("events received {} (DXGI {}, D3D9 {}, DxgKrnl {}), accepted as presents {}", c[0], c[1], c[2], c[3], c[4])
}

unsafe extern "system" fn on_event(rec: *mut EVENT_RECORD) {
    let r = unsafe { &*rec };
    let h = &r.EventHeader;
    let id = h.EventDescriptor.Id;
    COUNTS[0].fetch_add(1, Ordering::Relaxed);
    let which = if h.ProviderId == DXGI {
        1
    } else if h.ProviderId == D3D9 {
        2
    } else if h.ProviderId == DXGKRNL {
        3
    } else {
        0
    };
    if which > 0 {
        COUNTS[which].fetch_add(1, Ordering::Relaxed);
    }
    let src = if h.ProviderId == DXGI && id == 0x2A {
        PresentSource::Dxgi
    } else if h.ProviderId == D3D9 && id == 1 {
        PresentSource::D3d9
    } else if h.ProviderId == DXGKRNL && id == 0xB8 {
        PresentSource::Kernel
    } else {
        return;
    };
    if r.UserContext.is_null() {
        return;
    }
    COUNTS[4].fetch_add(1, Ordering::Relaxed);
    let tracker = unsafe { &*(r.UserContext as *const Mutex<PresentTracker>) };
    if let Ok(mut t) = tracker.lock() {
        t.record(h.ProcessId, src, qpc_to_ns(h.TimeStamp as u64));
    }
}

/// EVENT_TRACE_PROPERTIES followed by room for the session name, 8-byte aligned.
struct Props {
    buf: Vec<u64>,
}

impl Props {
    fn new() -> Props {
        let name_bytes = (SESSION_NAME.len() + 1) * 2;
        let total = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name_bytes;
        let mut p = Props { buf: vec![0u64; total.div_ceil(8)] };
        let ep = p.ptr();
        unsafe {
            (*ep).Wnode.BufferSize = total as u32;
            (*ep).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*ep).Wnode.ClientContext = 1; // QPC timestamps
            (*ep).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*ep).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            (*ep).BufferSize = 64; // KB
            (*ep).MinimumBuffers = 4;
            (*ep).MaximumBuffers = 32;
            (*ep).FlushTimer = 1; // seconds
        }
        p
    }
    fn ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
        self.buf.as_mut_ptr().cast()
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn check(e: WIN32_ERROR, what: &str) -> Result<(), String> {
    if e == ERROR_SUCCESS { Ok(()) } else { Err(format!("{what} failed: {}", windows::core::Error::from(e.to_hresult()).message())) }
}

/// Enable `provider` limited to the given event ids.
fn enable(handle: CONTROLTRACE_HANDLE, provider: &GUID, keywords: u64, ids: &[u16]) -> Result<(), String> {
    if std::env::var_os("FREEHM_ETW_NOFILTER").is_some() {
        // Diagnostics only: no event-id filter.
        let e = unsafe { EnableTraceEx2(handle, provider, EVENT_CONTROL_CODE_ENABLE_PROVIDER.0, TRACE_LEVEL_VERBOSE as u8, keywords, 0, 0, None) };
        return check(e, "EnableTraceEx2");
    }
    // EVENT_FILTER_EVENT_ID: BOOLEAN FilterIn; UCHAR Reserved; USHORT Count; USHORT Events[].
    let mut filter: Vec<u16> = vec![1, ids.len() as u16];
    filter.extend_from_slice(ids);
    let mut desc = EVENT_FILTER_DESCRIPTOR { Ptr: filter.as_ptr() as u64, Size: (filter.len() * 2) as u32, Type: EVENT_FILTER_TYPE_EVENT_ID };
    let params = ENABLE_TRACE_PARAMETERS { Version: ENABLE_TRACE_PARAMETERS_VERSION_2, EnableFilterDesc: &mut desc, FilterDescCount: 1, ..Default::default() };
    let e = unsafe { EnableTraceEx2(handle, provider, EVENT_CONTROL_CODE_ENABLE_PROVIDER.0, TRACE_LEVEL_VERBOSE as u8, keywords, 0, 0, Some(&params)) };
    check(e, "EnableTraceEx2")
}

struct Session {
    control: CONTROLTRACE_HANDLE,
    props: Props,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    fn start(tracker: Arc<Mutex<PresentTracker>>) -> Result<Session, String> {
        let name = wide(SESSION_NAME);
        let mut props = Props::new();
        let mut control = CONTROLTRACE_HANDLE::default();
        unsafe {
            let mut e = StartTraceW(&mut control, PCWSTR(name.as_ptr()), props.ptr());
            if e == ERROR_ALREADY_EXISTS {
                // Left over from a crashed instance: stop it and start fresh.
                let mut p2 = Props::new();
                let _ = ControlTraceW(CONTROLTRACE_HANDLE::default(), PCWSTR(name.as_ptr()), p2.ptr(), EVENT_TRACE_CONTROL_STOP);
                props = Props::new();
                e = StartTraceW(&mut control, PCWSTR(name.as_ptr()), props.ptr());
            }
            check(e, "StartTrace")?;
        }
        let mut s = Session { control, props, thread: None };
        // DXGI/D3D9: Analytic channel keyword + "Events"; DxgKrnl: Base | Present (as PresentMon).
        enable(control, &DXGI, 0x8000_0000_0000_0002, &[0x2A])?;
        if let Err(e) = enable(control, &D3D9, 0x8000_0000_0000_0002, &[1]) {
            log::warn!("D3D9 present events unavailable: {e}");
        }
        if let Err(e) = enable(control, &DXGKRNL, 0x0800_0001, &[0xB8]) {
            log::warn!("DxgKrnl present events unavailable: {e}");
        }

        let ctx = Arc::into_raw(tracker) as *mut std::ffi::c_void;
        let mut lname = wide(SESSION_NAME);
        let mut logfile = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(lname.as_mut_ptr()), Context: ctx, ..Default::default() };
        logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
        logfile.Anonymous2.EventRecordCallback = Some(on_event);
        let trace = unsafe { OpenTraceW(&mut logfile) };
        // INVALID_PROCESSTRACE_HANDLE is (TRACEHANDLE)INVALID_HANDLE_VALUE.
        if trace.Value == u64::MAX {
            unsafe { drop(Arc::from_raw(ctx as *const Mutex<PresentTracker>)) };
            return Err(format!("OpenTrace failed: {}", windows::core::Error::from_thread().message()));
        }
        let ctx_addr = ctx as usize;
        let trace_val = trace.Value;
        s.thread = Some(
            std::thread::Builder::new()
                .name("etw".into())
                .spawn(move || {
                    let h = PROCESSTRACE_HANDLE { Value: trace_val };
                    let e = unsafe { ProcessTrace(&[h], None, None) };
                    if e != ERROR_SUCCESS {
                        log::warn!("ProcessTrace ended: {e:?}");
                    }
                    unsafe {
                        let _ = CloseTrace(h);
                        drop(Arc::from_raw(ctx_addr as *const Mutex<PresentTracker>));
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        log::info!("frame-timing session started");
        Ok(s)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = ControlTraceW(self.control, PCWSTR::null(), self.props.ptr(), EVENT_TRACE_CONTROL_STOP);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        log::info!("frame-timing session stopped");
    }
}

pub struct FrameCollector {
    tracker: Arc<Mutex<PresentTracker>>,
    session: Option<Session>,
    last_request: Instant,
    error: Option<(String, Instant)>,
}

impl FrameCollector {
    pub fn new() -> FrameCollector {
        FrameCollector { tracker: Arc::new(Mutex::new(PresentTracker::default())), session: None, last_request: Instant::now(), error: None }
    }

    pub fn report(&mut self) -> FrameReport {
        self.last_request = Instant::now();
        if self.session.is_none() {
            if let Some((e, at)) = &self.error
                && at.elapsed() < Duration::from_secs(10)
            {
                return FrameReport { available: false, reason: Some(e.clone()), processes: Vec::new() };
            }
            match Session::start(self.tracker.clone()) {
                Ok(s) => {
                    self.session = Some(s);
                    self.error = None;
                }
                Err(e) => {
                    log::warn!("frame timing unavailable: {e}");
                    self.error = Some((e.clone(), Instant::now()));
                    return FrameReport { available: false, reason: Some(e), processes: Vec::new() };
                }
            }
        }
        let processes = self.tracker.lock().map(|mut t| t.snapshot(now_ns())).unwrap_or_default();
        FrameReport { available: true, reason: None, processes }
    }

    pub fn stop_if_idle(&mut self) {
        if self.session.is_some() && self.last_request.elapsed() >= IDLE_STOP {
            self.stop();
        }
    }

    pub fn stop(&mut self) {
        self.session = None;
        if let Ok(mut t) = self.tracker.lock() {
            *t = PresentTracker::default();
        }
    }
}
