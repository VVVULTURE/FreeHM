//! Test tool: opens a window and presents through a Direct3D 11 / DXGI flip-model swap
//! chain at vsync for N seconds (default 15), like a minimal game. Used to check that the
//! helper's ETW frame timing sees real `IDXGISwapChain::Present` calls. Falls back to the
//! WARP software rasterizer on machines without a GPU (CI runners).
//!
//! Usage: present_loop [seconds]

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

    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(15);
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("FreeHM.PresentLoop");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("FreeHM present loop"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            120,
            120,
            640,
            400,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        // Like a game being launched: take the foreground so game detection sees it.
        let _ = SetForegroundWindow(hwnd);
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC { Format: DXGI_FORMAT_B8G8R8A8_UNORM, ..Default::default() },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
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
        let back: ID3D11Texture2D = sc.GetBuffer(0)?;
        let mut rtv = None;
        dev.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
        let rtv = rtv.unwrap();
        println!("pid {} presenting for {secs} s", std::process::id());
        let start = Instant::now();
        let mut frames = 0u64;
        let mut msg = MSG::default();
        while start.elapsed() < Duration::from_secs(secs) {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let t = start.elapsed().as_secs_f32();
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.ClearRenderTargetView(&rtv, &[0.5 + 0.5 * t.sin(), 0.3, 0.5 + 0.5 * t.cos(), 1.0]);
            let _ = sc.Present(1, DXGI_PRESENT(0));
            frames += 1;
        }
        println!("presented {frames} frames ({:.1} fps)", frames as f64 / start.elapsed().as_secs_f64());
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}
