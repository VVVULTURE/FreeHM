//! FreeHM helper: a small Windows service that reads the values an unprivileged process
//! cannot — CPU package temperature (through the signed PawnIO driver) and per-process
//! present timing (through the system's graphics ETW providers) — and serves them over a
//! named pipe that accepts a single fixed, read-only request.
//!
//! Commands (`install`/`uninstall` need administrator rights):
//!   freehm-helper install [--log FILE]    install or repair the service and start it
//!   freehm-helper uninstall [--log FILE]  stop and remove the service
//!   freehm-helper status                  print whether the service is installed/running
//!   freehm-helper selftest                read sensors once and sample frames for 3 s (admin)
//!   freehm-helper service                 entry point used by the Service Control Manager

#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    std::process::exit(win::main());
    #[cfg(not(windows))]
    {
        eprintln!("freehm-helper is only used on Windows");
        std::process::exit(1);
    }
}
