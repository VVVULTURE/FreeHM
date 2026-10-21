//! One-time Linux sensor permission setup, run as root through `pkexec` from the settings
//! window (or `sudo freehm --setup-permissions --user NAME`).
//!
//! The only sensor FreeHM needs that an ordinary user cannot read is the RAPL package
//! energy counter (`/sys/class/powercap/intel-rapl:*/energy_uj`, used for CPU power on
//! Intel and AMD). Kernels since 5.10 make it root-only because fine-grained energy
//! readings leak information (CVE-2020-8694, "PLATYPUS"). Instead of making it
//! world-readable, the setup:
//!
//! 1. creates a system group `freehm` and adds the named user to it;
//! 2. installs `/etc/udev/rules.d/70-freehm.rules`, which gives that group read access
//!    (mode 0440, nothing writable) to the energy counters whenever they appear;
//! 3. applies the rule to the current counters.
//!
//! Group membership takes effect at the next login. `--remove-permissions` undoes all three.

use std::path::Path;
use std::process::Command;

pub const RULE_PATH: &str = "/etc/udev/rules.d/70-freehm.rules";
const GROUP: &str = "freehm";

const RULE: &str = "# Installed by FreeHM (freehm --setup-permissions). Remove with: freehm --remove-permissions\n\
# Lets members of the 'freehm' group read the RAPL energy counters (CPU power). Read-only.\n\
SUBSYSTEM==\"powercap\", KERNEL==\"intel-rapl*\", RUN+=\"/bin/sh -c 'chgrp freehm /sys%p/energy_uj && chmod 0440 /sys%p/energy_uj'\"\n";

pub fn rule_installed() -> bool {
    Path::new(RULE_PATH).exists()
}

/// POSIX portable user names (what `useradd` accepts by default).
pub fn valid_user_name(u: &str) -> bool {
    !u.is_empty()
        && u.len() <= 32
        && u.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && u.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(format!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())) }
}

fn group_exists() -> bool {
    Command::new("getent").args(["group", GROUP]).output().is_ok_and(|o| o.status.success())
}

fn user_exists(u: &str) -> bool {
    Command::new("getent").args(["passwd", u]).output().is_ok_and(|o| o.status.success())
}

/// The energy counter files present now.
fn energy_files() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/powercap") {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("intel-rapl") {
                let f = e.path().join("energy_uj");
                if f.exists() {
                    v.push(f);
                }
            }
        }
    }
    v
}

fn setup(user: &str) -> Result<String, String> {
    if !group_exists() {
        run("groupadd", &["--system", GROUP])?;
    }
    run("usermod", &["-aG", GROUP, user])?;
    let tmp = format!("{RULE_PATH}.tmp");
    std::fs::write(&tmp, RULE).map_err(|e| format!("{tmp}: {e}"))?;
    std::fs::rename(&tmp, RULE_PATH).map_err(|e| format!("{RULE_PATH}: {e}"))?;
    let _ = run("udevadm", &["control", "--reload-rules"]);
    let _ = run("udevadm", &["trigger", "--subsystem-match=powercap", "--action=change"]);
    // Apply directly as well, in case udev is not running (containers) or is slow.
    let files = energy_files();
    for f in &files {
        let p = f.to_string_lossy();
        run("chgrp", &[GROUP, &p])?;
        run("chmod", &["0440", &p])?;
    }
    let mut msg = format!("User '{user}' was added to the '{GROUP}' group and {RULE_PATH} was installed.");
    if files.is_empty() {
        msg.push_str(" This system currently exposes no RAPL energy counters, so CPU power stays unavailable.");
    } else {
        msg.push_str(" Log out and back in for CPU power to appear.");
    }
    Ok(msg)
}

fn remove(user: Option<&str>) -> Result<String, String> {
    match std::fs::remove_file(RULE_PATH) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{RULE_PATH}: {e}")),
    }
    let _ = run("udevadm", &["control", "--reload-rules"]);
    // Restore the kernel default (root:root 0400).
    for f in energy_files() {
        let p = f.to_string_lossy();
        let _ = run("chgrp", &["root", &p]);
        let _ = run("chmod", &["0400", &p]);
    }
    if group_exists() {
        if let Some(u) = user {
            let _ = run("gpasswd", &["-d", u, GROUP]);
        }
        run("groupdel", &[GROUP])?;
    }
    Ok(format!("Removed {RULE_PATH} and the '{GROUP}' group; RAPL counters are root-only again."))
}

/// `freehm --setup-permissions --user NAME` / `freehm --remove-permissions [--user NAME]`.
pub fn cli(cmd: &str, args: &[String]) -> i32 {
    let mut user: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--user" => user = it.next().cloned(),
            other => {
                eprintln!("unknown argument '{other}'");
                return 2;
            }
        }
    }
    if !rustix::process::geteuid().is_root() {
        eprintln!("this command must run as root: use the button in Settings ▸ Startup & permissions, or run it with sudo");
        return 1;
    }
    // Under pkexec, default to (and insist on) the user who asked for it.
    let caller = std::env::var("PKEXEC_UID").ok().and_then(|u| u.parse::<u32>().ok());
    if let Some(u) = &user {
        if !valid_user_name(u) || !user_exists(u) {
            eprintln!("'{u}' is not a valid user name on this system");
            return 2;
        }
        if let Some(uid) = caller
            && rustix::process::Uid::from_raw(uid) != uid_of(u).unwrap_or(rustix::process::Uid::ROOT)
        {
            eprintln!("--user must name the user who requested the change");
            return 2;
        }
    }
    let r = match cmd {
        "--setup-permissions" => match &user {
            Some(u) => setup(u),
            None => Err("--user NAME is required".into()),
        },
        _ => remove(user.as_deref()),
    };
    match r {
        Ok(m) => {
            println!("{m}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn uid_of(user: &str) -> Option<rustix::process::Uid> {
    let out = Command::new("id").args(["-u", user]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    Some(rustix::process::Uid::from_raw(s.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn user_names() {
        assert!(super::valid_user_name("vaden"));
        assert!(super::valid_user_name("_svc-1"));
        assert!(!super::valid_user_name("Root"));
        assert!(!super::valid_user_name("a b"));
        assert!(!super::valid_user_name("x;rm -rf /"));
        assert!(!super::valid_user_name(""));
        assert!(!super::valid_user_name("-x"));
    }

    #[test]
    fn rule_is_read_only() {
        assert!(super::RULE.contains("chmod 0440"));
        assert!(!super::RULE.contains("666") && !super::RULE.contains("a+r") && !super::RULE.contains("o+r"));
    }
}
