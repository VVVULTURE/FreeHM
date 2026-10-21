//! Platform actions triggered from the settings window: starting the app, opening folders,
//! choosing files, and the one-time (elevated) hardware-access setup.

use eframe::egui;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type Outcome = Arc<Mutex<Option<Result<String, String>>>>;

/// A long-running action (an elevated installer) executed off the UI thread.
#[derive(Default)]
pub struct Actions {
    running: Option<(String, Outcome)>,
    /// Outcome of the last finished action, shown in the permissions page.
    pub last: Option<(String, Result<String, String>)>,
    finished: bool,
}

impl Actions {
    pub fn busy(&self) -> Option<&str> {
        self.running.as_ref().map(|(n, _)| n.as_str())
    }

    /// Start `f` on a thread unless another action is running.
    pub fn spawn(&mut self, name: &str, ctx: &egui::Context, f: impl FnOnce() -> Result<String, String> + Send + 'static) {
        if self.running.is_some() {
            return;
        }
        let slot = Arc::new(Mutex::new(None));
        let s2 = slot.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = f();
            *s2.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        self.running = Some((name.to_string(), slot));
    }

    pub fn poll(&mut self, _ctx: &egui::Context) {
        let done = self.running.as_ref().and_then(|(_, s)| s.lock().unwrap().take());
        if let Some(r) = done {
            let (name, _) = self.running.take().unwrap();
            self.last = Some((name, r));
            self.finished = true;
        }
    }

    /// True once after an action completes (the caller then refreshes permission state).
    pub fn take_finished(&mut self) -> bool {
        std::mem::take(&mut self.finished)
    }
}

/// Start the background app (detached).
pub fn start_background() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--background").spawn();
    }
}

/// Open a folder (or file) with the desktop's default handler.
pub fn open_path(p: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(p).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
}

pub fn report_fatal(msg: &str) {
    #[cfg(windows)]
    unsafe {
        use crate::platform::windows::util::wide;
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        use windows::core::PCWSTR;
        let (t, m) = (wide("FreeHM"), wide(msg));
        MessageBoxW(None, PCWSTR(m.as_ptr()), PCWSTR(t.as_ptr()), MB_OK | MB_ICONERROR);
    }
    #[cfg(not(windows))]
    eprintln!("{msg}");
}

/// Ask the user for a file. `kind` is "font" or "exe".
pub fn browse_file(kind: &str) -> Option<String> {
    #[cfg(windows)]
    {
        use crate::platform::windows::util::{from_wide, wide};
        use windows::Win32::UI::Controls::Dialogs::*;
        use windows::core::PCWSTR;
        let filter: Vec<u16> = match kind {
            "font" => "Fonts (*.ttf;*.otf;*.ttc)\0*.ttf;*.otf;*.ttc\0All files\0*.*\0\0",
            _ => "Programs (*.exe)\0*.exe\0All files\0*.*\0\0",
        }
        .encode_utf16()
        .collect();
        let title = wide(if kind == "font" { "Choose a font file" } else { "Choose a game executable" });
        let mut buf = vec![0u16; 1024];
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: windows::core::PWSTR(buf.as_mut_ptr()),
            nMaxFile: buf.len() as u32,
            lpstrTitle: PCWSTR(title.as_ptr()),
            Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER,
            ..Default::default()
        };
        unsafe { GetOpenFileNameW(&mut ofn) }.as_bool().then(|| from_wide(&buf))
    }
    #[cfg(not(windows))]
    {
        let title = if kind == "font" { "Choose a font file" } else { "Choose a game executable" };
        let tries: [(&str, Vec<&str>); 2] =
            [("zenity", vec!["--file-selection", "--title", title]), ("kdialog", vec!["--getopenfilename", ".", "--title", title])];
        for (cmd, args) in tries {
            if let Ok(out) = std::process::Command::new(cmd).args(&args).output() {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    return (!s.is_empty()).then_some(s);
                }
                return None; // dialog exists but was cancelled
            }
        }
        None
    }
}

/// `freehm-helper.exe` next to this executable.
#[cfg(windows)]
pub fn helper_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) { "freehm-helper.exe" } else { "freehm-helper" };
    let p = exe.parent()?.join(name);
    p.exists().then_some(p)
}

/// Run `exe args` elevated (one UAC prompt) and wait for it. Returns its exit code.
#[cfg(windows)]
pub fn run_elevated(exe: &Path, args: &str) -> Result<u32, String> {
    use crate::platform::windows::util::wide;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::PCWSTR;
    let verb = wide("runas");
    let file = wide(&exe.display().to_string());
    let params = wide(args);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut sei).map_err(|e| {
            if e.code() == windows::Win32::Foundation::ERROR_CANCELLED.to_hresult() {
                "the administrator prompt was cancelled".to_string()
            } else {
                format!("could not start the elevated setup: {e}")
            }
        })?;
        if sei.hProcess.is_invalid() {
            return Err("the elevated setup did not start".into());
        }
        WaitForSingleObject(sei.hProcess, INFINITE);
        let mut code = 0u32;
        let _ = GetExitCodeProcess(sei.hProcess, &mut code);
        let _ = CloseHandle(sei.hProcess);
        Ok(code)
    }
}

/// Install/repair (`install`) or remove (`uninstall`) the Windows helper service.
#[cfg(windows)]
pub fn helper_command(cmd: &'static str) -> Result<String, String> {
    let exe = helper_exe().ok_or("freehm-helper.exe was not found next to freehm.exe; reinstall FreeHM")?;
    let log = std::env::temp_dir().join(format!("freehm-helper-{cmd}.log"));
    let args = format!("{cmd} --log \"{}\"", log.display());
    let code = run_elevated(&exe, &args)?;
    let detail = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    if code == 0 {
        Ok(if detail.trim().is_empty() { "done".into() } else { detail.trim().to_string() })
    } else {
        Err(format!("the helper {cmd} failed (exit code {code}). {}", detail.trim()))
    }
}

/// Linux: one-time permission setup through polkit (`pkexec`).
#[cfg(target_os = "linux")]
pub fn linux_permissions(remove: bool) -> Result<String, String> {
    // From an AppImage, run the image itself: root cannot see the user's FUSE mount.
    let exe = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from).map(Ok).unwrap_or_else(std::env::current_exe).map_err(|e| e.to_string())?;
    let user = std::env::var("USER").unwrap_or_default();
    let flag = if remove { "--remove-permissions" } else { "--setup-permissions" };
    let out = std::process::Command::new("pkexec")
        .arg(&exe)
        .arg(flag)
        .arg("--user")
        .arg(&user)
        .output()
        .map_err(|e| format!("could not run pkexec ({e}); run `sudo {} {flag} --user {user}` in a terminal instead", exe.display()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    match out.status.code() {
        Some(0) => Ok(text.trim().to_string()),
        Some(126) => Err("authorization was cancelled".into()),
        Some(127) => Err("not authorized (or polkit is not available)".into()),
        _ => Err(text.trim().to_string()),
    }
}
