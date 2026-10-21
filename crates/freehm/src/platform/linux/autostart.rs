//! XDG autostart: `$XDG_CONFIG_HOME/autostart/freehm.desktop`, honoured by GNOME, KDE,
//! Xfce, Cinnamon, MATE, LXQt and (via `dex`/`systemd-xdg-autostart-generator`) most
//! other sessions.

use freehm_core::ipc::AutostartStatus;
use std::path::PathBuf;

fn file() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("autostart").join("freehm.desktop")
}

/// The command to start FreeHM: the AppImage itself when running from one.
fn exec_path() -> Option<String> {
    if let Some(a) = std::env::var_os("APPIMAGE") {
        return Some(a.to_string_lossy().into_owned());
    }
    std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned())
}

fn quote(s: &str) -> String {
    // Desktop Entry spec: quote the argument and escape \ " ` $.
    let mut o = String::from("\"");
    for c in s.chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            o.push('\\');
        }
        o.push(c);
    }
    o.push('"');
    o
}

pub fn status() -> AutostartStatus {
    let f = file();
    match std::fs::read_to_string(&f) {
        Ok(s) => {
            let disabled = s.lines().any(|l| {
                let l = l.trim();
                l.eq_ignore_ascii_case("Hidden=true") || l.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
            });
            AutostartStatus { enabled: !disabled, mechanism: format!("XDG autostart ({})", f.display()), error: None }
        }
        Err(_) => AutostartStatus { enabled: false, mechanism: format!("XDG autostart ({})", f.display()), error: None },
    }
}

pub fn set(on: bool) -> Result<(), String> {
    let f = file();
    if !on {
        return match std::fs::remove_file(&f) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let exe = exec_path().ok_or("cannot determine the FreeHM executable path")?;
    let body = format!(
        "[Desktop Entry]\nType=Application\nName=FreeHM\nComment=Hardware statistics overlay\nExec={} --background\nIcon=io.github.vvvulture.FreeHM\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\nX-KDE-autostart-after=panel\n",
        quote(&exe)
    );
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = f.with_extension("desktop.tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &f).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn exec_quoting() {
        assert_eq!(super::quote("/opt/Free HM/freehm"), "\"/opt/Free HM/freehm\"");
        assert_eq!(super::quote("/a/$b`c\"d"), "\"/a/\\$b\\`c\\\"d\"");
    }
}
