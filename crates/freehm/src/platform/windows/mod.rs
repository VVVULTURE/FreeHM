//! Windows platform layer.

pub mod autostart;
pub mod helper_client;
pub mod ipc;
pub mod overlay;
pub mod pdh;
pub mod probe;
pub mod sensors;
pub mod tray;
pub mod util;

use crate::app::{App, AppEvent, EventSink, Placement, Platform, TrayState};
use crate::sensors::nvml::NvmlProvider;
use freehm_core::config::{Config, HelperUse};
use freehm_core::hotkey::{Hotkey, Key};
use freehm_core::ipc::{AutostartStatus, HelperStatus, MonitorInfo};
use freehm_core::render::Pixmap;
use freehm_core::telemetry::Provider;
use helper_client::{HelperProvider, HelperShared};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use util::wide;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

const WM_APP_WAKE: u32 = WM_APP + 2;
const HOTKEY_ID: i32 = 1;
const TIMER_TICK: usize = 1;

thread_local! {
    static PENDING: RefCell<VecDeque<AppEvent>> = const { RefCell::new(VecDeque::new()) };
    static TRAY: RefCell<Option<tray::Tray>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static TICK: Cell<bool> = const { Cell::new(false) };
}

/// Queue an event from a window procedure / hook callback on the UI thread.
pub(crate) fn push_event(e: AppEvent) {
    PENDING.with(|p| p.borrow_mut().push_back(e));
}

unsafe extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY if wp.0 as i32 == HOTKEY_ID => push_event(AppEvent::Hotkey),
        tray::WM_TRAY => {
            let action = TRAY.with(|t| t.try_borrow().ok().and_then(|t| t.as_ref().and_then(|t| t.on_message(wp, lp))));
            if let Some(a) = action {
                push_event(AppEvent::Tray(a));
            }
        }
        WM_POWERBROADCAST => {
            // PBT_APMRESUMEAUTOMATIC
            if wp.0 == 0x12 {
                push_event(AppEvent::Resumed);
            }
            return LRESULT(1);
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => push_event(AppEvent::DisplayChanged),
        WM_SETTINGCHANGE if wp.0 == SPI_SETWORKAREA.0 as usize => push_event(AppEvent::DisplayChanged),
        WM_TIMER if wp.0 == TIMER_TICK => TICK.with(|t| t.set(true)),
        WM_QUERYENDSESSION => return LRESULT(1),
        WM_ENDSESSION if wp.0 != 0 => push_event(AppEvent::Quit),
        WM_APP_WAKE => {}
        WM_CLOSE => return LRESULT(0),
        m if m != 0 && m == TASKBAR_CREATED.with(|c| c.get()) => {
            // Explorer restarted: our icon is gone, add it again.
            TRAY.with(|t| {
                if let Ok(mut t) = t.try_borrow_mut()
                    && let Some(t) = t.as_mut()
                {
                    t.add();
                }
            });
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

unsafe extern "system" fn foreground_hook(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    push_event(AppEvent::ForegroundChanged);
}

/// Holds the single-instance mutex for the lifetime of the process.
pub struct InstanceGuard(#[allow(dead_code)] util::OwnedHandle);

pub fn single_instance() -> Option<InstanceGuard> {
    let name = wide(&format!("Local\\FreeHM.Instance.{}", util::current_user_sid().unwrap_or_default()));
    unsafe {
        let h = CreateMutexW(None, false, PCWSTR(name.as_ptr())).ok()?;
        if windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS {
            let _ = util::OwnedHandle(h);
            return None;
        }
        Some(InstanceGuard(util::OwnedHandle(h)))
    }
}

/// Single instance of the settings window. When one is already open, bring it to the
/// front and return `None`.
pub fn settings_instance() -> Option<InstanceGuard> {
    let name = wide(&format!("Local\\FreeHM.Settings.{}", util::current_user_sid().unwrap_or_default()));
    unsafe {
        let h = CreateMutexW(None, false, PCWSTR(name.as_ptr())).ok()?;
        if windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS {
            let _ = util::OwnedHandle(h);
            let title = wide("FreeHM Settings");
            if let Ok(w) = FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) {
                if IsIconic(w).as_bool() {
                    let _ = ShowWindow(w, SW_RESTORE);
                }
                let _ = SetForegroundWindow(w);
            }
            return None;
        }
        Some(InstanceGuard(util::OwnedHandle(h)))
    }
}

pub fn set_dpi_awareness() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn ipc_connect() -> std::io::Result<Box<dyn crate::ipc::ReadWrite + Send>> {
    Ok(Box::new(ipc::connect()?))
}

struct WinPlatform {
    hwnd: HWND,
    overlay: overlay::Overlay,
    hotkey_registered: bool,
    settings_child: Option<std::process::Child>,
    helper_shared: Arc<Mutex<HelperShared>>,
    helper_enabled: Arc<AtomicBool>,
}

fn vk_for(k: Key) -> u32 {
    match k {
        Key::Letter(c) | Key::Digit(c) => c as u32,
        Key::F(n) => VK_F1.0 as u32 + (n as u32 - 1),
        Key::Insert => VK_INSERT.0 as u32,
        Key::Delete => VK_DELETE.0 as u32,
        Key::Home => VK_HOME.0 as u32,
        Key::End => VK_END.0 as u32,
        Key::PageUp => VK_PRIOR.0 as u32,
        Key::PageDown => VK_NEXT.0 as u32,
        Key::Pause => VK_PAUSE.0 as u32,
        Key::ScrollLock => VK_SCROLL.0 as u32,
        Key::PrintScreen => VK_SNAPSHOT.0 as u32,
        Key::Backquote => VK_OEM_3.0 as u32,
    }
}

fn focus_process_window(pid: u32) -> bool {
    struct Find {
        pid: u32,
        hwnd: Option<HWND>,
    }
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        let f = unsafe { &mut *(lp.0 as *mut Find) };
        let mut p = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
        if p == f.pid && unsafe { IsWindowVisible(h) }.as_bool() {
            f.hwnd = Some(h);
            return false.into();
        }
        true.into()
    }
    let mut f = Find { pid, hwnd: None };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut f as *mut Find as isize));
        if let Some(h) = f.hwnd {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
            return true;
        }
    }
    false
}

impl Platform for WinPlatform {
    fn overlay_backend(&self) -> String {
        "Win32 layered window (topmost, click-through, no-activate)".into()
    }

    fn overlay_unsupported(&self) -> Option<String> {
        None
    }

    fn monitors(&mut self) -> Vec<MonitorInfo> {
        overlay::monitors()
    }

    fn overlay_show(&mut self, pm: &Pixmap, place: &Placement) {
        self.overlay.show(pm, place);
    }

    fn overlay_hide(&mut self) {
        self.overlay.hide();
    }

    fn overlay_set_capture_excluded(&mut self, on: bool) {
        self.overlay.set_capture_excluded(on);
    }

    fn overlay_raise(&mut self) {
        self.overlay.raise();
    }

    fn register_hotkey(&mut self, hk: &Hotkey) -> Result<String, String> {
        unsafe {
            if self.hotkey_registered {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_ID);
                self.hotkey_registered = false;
            }
            let mut mods = MOD_NOREPEAT;
            if hk.ctrl {
                mods |= MOD_CONTROL;
            }
            if hk.shift {
                mods |= MOD_SHIFT;
            }
            if hk.alt {
                mods |= MOD_ALT;
            }
            if hk.meta {
                mods |= MOD_WIN;
            }
            match RegisterHotKey(Some(self.hwnd), HOTKEY_ID, mods, vk_for(hk.key)) {
                Ok(()) => {
                    self.hotkey_registered = true;
                    Ok("RegisterHotKey".into())
                }
                Err(e) if e.code() == windows::Win32::Foundation::ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() => {
                    Err(format!("{hk} is already used by another application or by Windows; choose a different shortcut"))
                }
                Err(e) => Err(format!("RegisterHotKey failed: {e}")),
            }
        }
    }

    fn update_tray(&mut self, st: &TrayState) {
        TRAY.with(|t| {
            if let Ok(mut t) = t.try_borrow_mut()
                && let Some(t) = t.as_mut()
            {
                t.update(st);
            }
        });
    }

    fn autostart_status(&self) -> AutostartStatus {
        autostart::status()
    }

    fn set_autostart(&mut self, on: bool) -> Result<(), String> {
        autostart::set(on)
    }

    fn open_settings(&mut self) -> Result<(), String> {
        if let Some(c) = self.settings_child.as_mut()
            && matches!(c.try_wait(), Ok(None))
        {
            focus_process_window(c.id());
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let child = std::process::Command::new(exe).arg("--settings").spawn().map_err(|e| e.to_string())?;
        unsafe {
            let _ = AllowSetForegroundWindow(child.id());
        }
        self.settings_child = Some(child);
        Ok(())
    }

    fn helper_status(&mut self) -> HelperStatus {
        let svc = helper_client::service_info();
        let sh = self.helper_shared.lock().unwrap();
        let message = if !self.helper_enabled.load(Ordering::Relaxed) {
            "disabled in settings".to_string()
        } else if !svc.installed {
            "not installed — CPU temperature and FPS/frame time are unavailable".to_string()
        } else if !svc.running {
            "installed but not running".to_string()
        } else if sh.message.is_empty() {
            "running (not contacted yet — it is used only while CPU temperature, CPU power or FPS is displayed)".into()
        } else {
            sh.message.clone()
        };
        HelperStatus {
            applicable: true,
            installed: svc.installed,
            running: svc.running,
            connected: sh.connected,
            message,
            pawnio_installed: helper_client::pawnio_installed(),
        }
    }

    fn platform_name(&self) -> String {
        "Windows".into()
    }

    fn on_config(&mut self, cfg: &Config) {
        self.helper_enabled.store(cfg.hardware.use_helper == HelperUse::Enabled, Ordering::Relaxed);
    }

    fn request_exit(&mut self) {
        unsafe { PostQuitMessage(0) };
    }
}

/// Run the background application until Quit. Returns the process exit code.
pub fn run(cfg: Config, cfg_path: std::path::PathBuf, notes: Vec<String>, mock: bool, open_settings: bool) -> i32 {
    unsafe {
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(e) => {
                log::error!("GetModuleHandle: {e}");
                return 1;
            }
        };
        let class = wide("FreeHM.Main");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let title = wide("FreeHM");
        // A hidden top-level window (not message-only) so that broadcasts such as
        // TaskbarCreated, WM_DISPLAYCHANGE and WM_POWERBROADCAST reach us.
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log::error!("CreateWindow: {e}");
                return 1;
            }
        };
        let tc = wide("TaskbarCreated");
        TASKBAR_CREATED.with(|c| c.set(RegisterWindowMessageW(PCWSTR(tc.as_ptr()))));

        let (tx, rx) = mpsc::channel::<AppEvent>();
        let hwnd_val = hwnd.0 as isize;
        let sink = EventSink::new(
            tx,
            Arc::new(move || {
                let _ = PostMessageW(Some(HWND(hwnd_val as *mut _)), WM_APP_WAKE, WPARAM(0), LPARAM(0));
            }),
        );

        if let Err(e) = ipc::start_server(sink.clone()) {
            log::error!("IPC endpoint unavailable ({e}); is another FreeHM instance running?");
            return 2;
        }

        let helper_shared = Arc::new(Mutex::new(HelperShared::default()));
        let helper_enabled = Arc::new(AtomicBool::new(cfg.hardware.use_helper == HelperUse::Enabled));
        let providers: Vec<Box<dyn Provider>> = if mock {
            vec![Box::new(freehm_core::telemetry::mock::MockProvider::demo().0)]
        } else {
            // Order = precedence when two providers supply the same metric.
            vec![
                Box::new(NvmlProvider::new()),
                Box::new(sensors::WddmGpuProvider::new()),
                Box::new(sensors::CpuProvider::new()),
                Box::new(sensors::MemoryProvider),
                Box::new(HelperProvider::new(helper_shared.clone(), helper_enabled.clone())),
            ]
        };
        let snap_sink = sink.clone();
        let telemetry = freehm_core::telemetry::TelemetryThread::spawn(providers, Box::new(move |s| snap_sink.send(AppEvent::Snapshot(s))));
        let detector = crate::detect::spawn(Box::new(probe::WinProbe::default()), cfg.detection.clone(), sink.clone());

        // Out-of-context WinEvent hook: callbacks run in our process; nothing is loaded
        // into other processes.
        const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
        const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
        const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_hook),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );

        let overlay = match overlay::Overlay::create() {
            Ok(o) => o,
            Err(e) => {
                log::error!("could not create overlay window: {e}");
                return 1;
            }
        };
        TRAY.with(|t| *t.borrow_mut() = Some(tray::Tray::new(hwnd)));
        let _ = SetTimer(Some(hwnd), TIMER_TICK, 1000, None);

        let mut plat = WinPlatform { hwnd, overlay, hotkey_registered: false, settings_child: None, helper_shared, helper_enabled };
        let mut app = App::new(cfg, cfg_path, notes, telemetry, detector, &mut plat);
        log::info!("FreeHM {} running", freehm_core::APP_VERSION);
        if open_settings {
            app.handle(AppEvent::Tray(crate::app::TrayAction::OpenSettings), &mut plat);
        }

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 || r.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            // Process everything queued by window procedures and background threads.
            loop {
                let ev = PENDING.with(|p| p.borrow_mut().pop_front()).or_else(|| rx.try_recv().ok());
                let Some(ev) = ev else { break };
                app.handle(ev, &mut plat);
            }
            if TICK.with(|t| t.replace(false)) {
                app.tick(&mut plat);
            }
            if app.is_exiting() {
                break;
            }
        }

        // Orderly shutdown: tray icon removed, hotkey released, overlay destroyed.
        let _ = KillTimer(Some(hwnd), TIMER_TICK);
        if !hook.is_invalid() {
            let _ = UnhookWinEvent(hook);
        }
        if plat.hotkey_registered {
            let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
        }
        TRAY.with(|t| t.borrow_mut().take());
        drop(app);
        drop(plat);
        let _ = DestroyWindow(hwnd);
        0
    }
}
