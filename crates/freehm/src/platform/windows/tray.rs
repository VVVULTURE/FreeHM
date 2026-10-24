//! Notification-area (tray) icon using `Shell_NotifyIconW` with `NOTIFYICON_VERSION_4`.
//! Left click (or Enter/Space) opens settings; right click shows the menu. The icon is
//! re-added when Explorer restarts (`TaskbarCreated`) and removed on exit, so no stale
//! icons are left behind.

use super::util::wide;
use crate::app::{TrayAction, TrayState};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

// shellapi.h: NIN_SELECT = WM_USER, NIN_KEYSELECT = NIN_SELECT | NINF_KEY.
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER | 1;

pub const WM_TRAY: u32 = WM_APP + 1;
const ID_OPEN: usize = 1;
const ID_TOGGLE: usize = 2;
const ID_DETECT: usize = 3;
const ID_AUTOSTART: usize = 4;
const ID_QUIT: usize = 5;

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    pub state: TrayState,
    added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        let icon = make_icon(32).unwrap_or_default();
        let mut t =
            Tray { hwnd, icon, state: TrayState { overlay_visible: false, detection_enabled: true, autostart: true, tooltip: "FreeHM".into() }, added: false };
        t.add();
        t
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        let tip: Vec<u16> = self.state.tooltip.encode_utf16().take(127).collect();
        d.szTip[..tip.len()].copy_from_slice(&tip);
        d
    }

    /// (Re-)add the icon. Called at start and after Explorer restarts.
    pub fn add(&mut self) {
        let mut d = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            self.added = Shell_NotifyIconW(NIM_ADD, &d).as_bool();
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
        if !self.added {
            log::warn!("could not add the tray icon (Explorer not running?); it will be added when the taskbar appears");
        }
    }

    pub fn update(&mut self, st: &TrayState) {
        if *st == self.state {
            return;
        }
        let tip_changed = st.tooltip != self.state.tooltip;
        self.state = st.clone();
        if tip_changed && self.added {
            let d = self.data();
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
            }
        }
    }

    /// Show a notification balloon/toast from the icon. Windows holds it back while a
    /// fullscreen program is in front and shows it (or files it) afterwards.
    pub fn notify(&self, title: &str, text: &str) {
        if !self.added {
            return;
        }
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_INFO;
        let t: Vec<u16> = title.encode_utf16().take(63).collect();
        d.szInfoTitle[..t.len()].copy_from_slice(&t);
        let b: Vec<u16> = text.encode_utf16().take(255).collect();
        d.szInfo[..b.len()].copy_from_slice(&b);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            let d = self.data();
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            }
            self.added = false;
        }
    }

    /// Handle the tray callback message; returns the chosen action, if any.
    pub fn on_message(&self, wp: WPARAM, lp: LPARAM) -> Option<TrayAction> {
        let event = (lp.0 as u32) & 0xFFFF;
        match event {
            NIN_SELECT | NIN_KEYSELECT => Some(TrayAction::OpenSettings),
            WM_CONTEXTMENU => {
                // With NOTIFYICON_VERSION_4 the anchor point is in wParam.
                let x = (wp.0 & 0xFFFF) as i16 as i32;
                let y = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
                self.menu(POINT { x, y })
            }
            _ => None,
        }
    }

    fn menu(&self, pt: POINT) -> Option<TrayAction> {
        unsafe {
            let m = CreatePopupMenu().ok()?;
            let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
            let items: [(usize, &str, MENU_ITEM_FLAGS); 4] = [
                (ID_OPEN, "Open settings", MF_STRING | MF_DEFAULT),
                (ID_TOGGLE, "Show overlay\tShortcut", MF_STRING | check(self.state.overlay_visible)),
                (ID_DETECT, "Automatic game detection", MF_STRING | check(self.state.detection_enabled)),
                (ID_AUTOSTART, "Start with Windows", MF_STRING | check(self.state.autostart)),
            ];
            for (id, text, flags) in items {
                let w = wide(text);
                let _ = AppendMenuW(m, flags, id, PCWSTR(w.as_ptr()));
            }
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            let q = wide("Quit FreeHM");
            let _ = AppendMenuW(m, MF_STRING, ID_QUIT, PCWSTR(q.as_ptr()));
            // Required so the menu closes when the user clicks elsewhere (documented quirk).
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_BOTTOMALIGN, pt.x, pt.y, None, self.hwnd, None);
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(m);
            match cmd.0 as usize {
                ID_OPEN => Some(TrayAction::OpenSettings),
                ID_TOGGLE => Some(TrayAction::ToggleOverlay),
                ID_DETECT => Some(TrayAction::ToggleDetection),
                ID_AUTOSTART => Some(TrayAction::ToggleAutostart),
                ID_QUIT => Some(TrayAction::Quit),
                _ => None,
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        if !self.icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(self.icon);
            }
        }
    }
}

fn make_icon(size: u32) -> Option<HICON> {
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let px = crate::icon::icon_pixels(size);
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits.cast::<u8>(), px.len());
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
