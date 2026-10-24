//! Test tool: opens a window and presents through a Direct3D 11 / DXGI flip-model swap
//! chain at vsync for N seconds (default 15), like a minimal game. Used to check that the
//! helper's ETW frame timing sees real `IDXGISwapChain::Present` calls. Falls back to the
//! WARP software rasterizer on machines without a GPU (CI runners).
//!
//! `--fullscreen` switches the swap chain to exclusive fullscreen
//! (`IDXGISwapChain::SetFullscreenState`), as games with an "exclusive fullscreen" option do;
//! `--blt` uses the legacy bit-block-transfer swap effect instead of flip model. `--native`
//! covers the primary monitor at its current resolution and refresh rate (no display mode
//! change, like most games); `--hz N` asks for a different refresh rate.
//!
//! Usage: present_loop [seconds] [--fullscreen] [--blt] [--native] [--hz N]

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // Like games: ignore the Alt-key menu. A lone Alt press would otherwise enter the
        // window-menu modal loop and stop presenting (the smoke test taps Alt to move focus).
        if m == WM_SYSCOMMAND && (w.0 & 0xFFF0) == SC_KEYMENU as usize {
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs: u64 = args.iter().find_map(|s| s.parse().ok()).unwrap_or(15);
    let fullscreen = args.iter().any(|a| a == "--fullscreen");
    let blt = args.iter().any(|a| a == "--blt");
    let native = args.iter().any(|a| a == "--native");
    let hz: Option<u32> = args.iter().position(|a| a == "--hz").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok());
    // The current display mode of the primary monitor.
    let mut mode =
        windows::Win32::Graphics::Gdi::DEVMODEW { dmSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::DEVMODEW>() as u16, ..Default::default() };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::EnumDisplaySettingsW(None, windows::Win32::Graphics::Gdi::ENUM_CURRENT_SETTINGS, &mut mode);
    }
    let (mw, mh, mhz) = (mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency);
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("FreeHM.PresentLoop");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("FreeHM present loop"),
            if native { WS_POPUP | WS_VISIBLE } else { WS_OVERLAPPEDWINDOW | WS_VISIBLE },
            if native { 0 } else { 120 },
            if native { 0 } else { 120 },
            if native { mw as i32 } else { 640 },
            if native { mh as i32 } else { 400 },
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        // Like a game being launched: take the foreground so game detection sees it.
        let _ = SetForegroundWindow(hwnd);
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Width: if native { mw } else { 0 },
                Height: if native { mh } else { 0 },
                RefreshRate: DXGI_RATIONAL { Numerator: hz.unwrap_or(if native { mhz } else { 0 }), Denominator: 1 },
                ..Default::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: true.into(),
            SwapEffect: if blt { DXGI_SWAP_EFFECT_DISCARD } else { DXGI_SWAP_EFFECT_FLIP_DISCARD },
            Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0 as u32,
        };
        let (mut sc, mut dev, mut ctx) = (None, None, None);
        let mut ok = false;
        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            if D3D11CreateDeviceAndSwapChain(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&desc),
                Some(&mut sc),
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )
            .is_ok()
            {
                println!("device: {}", if driver == D3D_DRIVER_TYPE_HARDWARE { "hardware" } else { "WARP" });
                ok = true;
                break;
            }
        }
        if !ok {
            eprintln!("could not create a Direct3D 11 device");
            std::process::exit(1);
        }
        let (sc, dev, ctx): (IDXGISwapChain, ID3D11Device, ID3D11DeviceContext) = (sc.unwrap(), dev.unwrap(), ctx.unwrap());
        let target = |sc: &IDXGISwapChain| -> windows::core::Result<ID3D11RenderTargetView> {
            let back: ID3D11Texture2D = sc.GetBuffer(0)?;
            let mut rtv = None;
            dev.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            Ok(rtv.unwrap())
        };
        let mut rtv = Some(target(&sc)?);
        let client = || {
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            (r.right - r.left, r.bottom - r.top)
        };
        let mut size = client();
        if fullscreen {
            match sc.SetFullscreenState(true, None) {
                Ok(()) => {
                    println!("exclusive fullscreen: on");
                    // Flip model requires ResizeBuffers after every fullscreen transition.
                    size = (-1, -1);
                }
                Err(e) => println!("exclusive fullscreen: FAILED ({e})"),
            }
        }
        println!("pid {} hwnd {:#x} presenting for {secs} s", std::process::id(), hwnd.0 as usize);
        let start = Instant::now();
        let mut frames = 0u64;
        let mut last_hr = windows::core::HRESULT(0);
        let mut msg = MSG::default();
        while start.elapsed() < Duration::from_secs(secs) {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // Entering fullscreen resizes the window (and flip model needs new buffers even
            // when the size stays); the buffers must follow.
            if client() != size {
                size = client();
                drop(rtv.take()); // ResizeBuffers needs every reference to the old buffers released
                ctx.OMSetRenderTargets(None, None);
                sc.ResizeBuffers(0, 0, 0, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH)?;
                rtv = Some(target(&sc)?);
            }
            let rtv = rtv.as_ref().unwrap();
            let t = start.elapsed().as_secs_f32();
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.ClearRenderTargetView(rtv, &[0.5 + 0.5 * t.sin(), 0.3, 0.5 + 0.5 * t.cos(), 1.0]);
            let hr = sc.Present(1, DXGI_PRESENT(0));
            if hr != last_hr {
                println!("{:.2} s: Present returned {hr:?}", start.elapsed().as_secs_f32());
                last_hr = hr;
            }
            frames += 1;
        }
        println!("presented {frames} frames ({:.1} fps)", frames as f64 / start.elapsed().as_secs_f64());
        if fullscreen {
            let mut on = windows::core::BOOL(0);
            let _ = sc.GetFullscreenState(Some(&mut on), None);
            println!("still exclusive fullscreen at the end: {}", on.as_bool());
            let _ = sc.SetFullscreenState(false, None);
        }
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}
