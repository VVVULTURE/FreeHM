//! Test tool: shows a solid green 200×100 topmost window at (100, 100) for N seconds and
//! keeps it at the top of the z-order, to check which kinds of window stay visible above a
//! game in exclusive fullscreen.
//!
//! Usage: topmost_probe <plain|layered|ulw|layered-hit|ulw-hit> [seconds] [x y]

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if m == WM_PAINT {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(h, &mut ps);
                let br = CreateSolidBrush(COLORREF(0x0000FF00));
                FillRect(dc, &ps.rcPaint, br);
                let _ = DeleteObject(br.into());
                let _ = EndPaint(h, &ps);
            }
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let kind = args.first().map(String::as_str).unwrap_or("plain").to_string();
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15);
    let x: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
    let y: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("FreeHM.TopmostProbe");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let mut ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        // "-hit" variants keep mouse input (no WS_EX_TRANSPARENT).
        if kind != "plain" {
            ex |= WS_EX_LAYERED;
        }
        if kind != "plain" && !kind.ends_with("-hit") {
            ex |= WS_EX_TRANSPARENT;
        }
        let kind = kind.trim_end_matches("-hit").to_string();
        let hwnd = CreateWindowExW(ex, class, w!("probe"), WS_POPUP, x, y, 200, 100, None, None, Some(hinst.into()), None)?;
        match kind.as_str() {
            "layered" => {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            }
            "ulw" => {
                // Per-pixel alpha through UpdateLayeredWindow, like the FreeHM overlay.
                let screen = GetDC(None);
                let mem = CreateCompatibleDC(Some(screen));
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: 200,
                        biHeight: -100,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
                let bmp = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
                SelectObject(mem, bmp.into());
                let px = std::slice::from_raw_parts_mut(bits.cast::<u32>(), 200 * 100);
                px.fill(0xFF00FF00);
                let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
                UpdateLayeredWindow(
                    hwnd,
                    None,
                    Some(&POINT { x, y }),
                    Some(&SIZE { cx: 200, cy: 100 }),
                    Some(mem),
                    Some(&POINT { x: 0, y: 0 }),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                )?;
                ReleaseDC(None, screen);
            }
            _ => {}
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        println!("{kind} probe window {:#x} for {secs} s", hwnd.0 as usize);
        let start = Instant::now();
        let mut msg = MSG::default();
        while start.elapsed() < Duration::from_secs(secs) {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}
