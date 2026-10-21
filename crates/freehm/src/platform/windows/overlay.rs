//! The overlay window: an ordinary top-level window in FreeHM's own process.
//!
//! - `WS_EX_LAYERED` + `UpdateLayeredWindow` with a premultiplied 32-bit DIB: per-pixel
//!   alpha, composed by DWM; nothing is drawn unless the content changes.
//! - `WS_EX_TRANSPARENT` (plus `HTTRANSPARENT` hit-testing): mouse input passes through.
//! - `WS_EX_NOACTIVATE` + `SW_SHOWNOACTIVATE`: never takes keyboard focus from the game.
//! - `WS_EX_TOPMOST`, re-asserted when the foreground window changes.
//! - `WS_EX_TOOLWINDOW`: no taskbar button or Alt+Tab entry.
//!
//! It is not injected into, attached to, or parented by any game window.

use super::util::wide;
use crate::app::Placement;
use freehm_core::render::Pixmap;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

pub const CLASS: &str = "FreeHM.Overlay";

pub struct Overlay {
    hwnd: HWND,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    bmp_w: u32,
    bmp_h: u32,
    shown: bool,
    last_pos: (i32, i32),
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // Belt and braces with WS_EX_TRANSPARENT: never claim mouse input.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DPICHANGED => {
            super::push_event(crate::app::AppEvent::DisplayChanged);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

impl Overlay {
    pub fn create() -> windows::core::Result<Overlay> {
        unsafe {
            let hinst = GetModuleHandleW(None)?;
            let class = wide(CLASS);
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let title = wide("FreeHM Overlay");
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hinst.into()),
                None,
            )?;
            let screen = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            Ok(Overlay {
                hwnd,
                mem_dc,
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                bits: std::ptr::null_mut(),
                bmp_w: 0,
                bmp_h: 0,
                shown: false,
                last_pos: (i32::MIN, i32::MIN),
            })
        }
    }

    fn ensure_bitmap(&mut self, w: u32, h: u32) -> bool {
        if self.bmp_w == w && self.bmp_h == h && !self.bits.is_null() {
            return true;
        }
        unsafe {
            if !self.bitmap.is_invalid() {
                SelectObject(self.mem_dc, self.old);
                let _ = DeleteObject(self.bitmap.into());
                self.bitmap = HBITMAP::default();
                self.bits = std::ptr::null_mut();
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down rows, same order as Pixmap
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            match CreateDIBSection(Some(self.mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => {
                    self.bitmap = b;
                    self.old = SelectObject(self.mem_dc, b.into());
                    self.bits = bits.cast();
                    self.bmp_w = w;
                    self.bmp_h = h;
                    true
                }
                Err(e) => {
                    log::error!("CreateDIBSection {w}x{h} failed: {e}");
                    false
                }
            }
        }
    }

    pub fn show(&mut self, pm: &Pixmap, place: &Placement) {
        if !self.ensure_bitmap(pm.width, pm.height) {
            return;
        }
        unsafe {
            // Both buffers are premultiplied BGRA with identical layout.
            std::ptr::copy_nonoverlapping(pm.data.as_ptr(), self.bits, pm.data.len());
            let _ = GdiFlush();
            let pos = POINT { x: place.x, y: place.y };
            let size = SIZE { cx: pm.width as i32, cy: pm.height as i32 };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            if let Err(e) = UpdateLayeredWindow(self.hwnd, None, Some(&pos), Some(&size), Some(self.mem_dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA) {
                log::warn!("UpdateLayeredWindow failed: {e}");
                return;
            }
            if !self.shown {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                self.shown = true;
                self.raise();
            }
            self.last_pos = (place.x, place.y);
        }
    }

    pub fn hide(&mut self) {
        if self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.shown = false;
        }
    }

    /// Put the overlay back at the top of the topmost band without activating it.
    pub fn raise(&self) {
        if self.shown {
            unsafe {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
            }
        }
    }

    pub fn set_capture_excluded(&self, on: bool) {
        let aff = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        if let Err(e) = unsafe { SetWindowDisplayAffinity(self.hwnd, aff) } {
            log::warn!("SetWindowDisplayAffinity failed: {e}");
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            if !self.bitmap.is_invalid() {
                SelectObject(self.mem_dc, self.old);
                let _ = DeleteObject(self.bitmap.into());
            }
            let _ = DeleteDC(self.mem_dc);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Enumerate monitors with physical bounds and effective DPI scale.
pub fn monitors() -> Vec<freehm_core::ipc::MonitorInfo> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::core::BOOL;
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let v = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        v.push(m);
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut handles as *mut _ as isize));
    }
    let mut out = Vec::new();
    for h in handles {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !unsafe { GetMonitorInfoW(h, &mut mi.monitorInfo) }.as_bool() {
            continue;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
        let r = mi.monitorInfo.rcMonitor;
        out.push(freehm_core::ipc::MonitorInfo {
            name: super::util::from_wide(&mi.szDevice),
            primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            x: r.left,
            y: r.top,
            width: (r.right - r.left) as u32,
            height: (r.bottom - r.top) as u32,
            scale: dx as f32 / 96.0,
        });
    }
    out
}
