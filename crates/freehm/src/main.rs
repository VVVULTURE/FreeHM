// No console window on Windows; CLI commands attach to the parent console when present.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod detect;
mod fonts;
mod icon;
mod ipc;
mod logging;
mod paths;
mod platform;
mod sensors;
mod settings;

use freehm_core::config::Config;
use freehm_core::ipc::{Request, Response};

const HELP: &str = "FreeHM — lightweight hardware statistics overlay

Usage: freehm [command]

  (no command)     Start in the background (or open settings if already running)
  --background     Start in the background without opening settings
  --settings       Open the settings window
  --toggle         Show/hide the overlay of the running instance
  --show | --hide  Force the overlay on/off until the active game changes
  --quit           Quit the running instance
  --status         Print the running instance's status as JSON
  --apply-config FILE
                   Validate a configuration file (same format as config.json; missing
                   fields take their defaults), apply it to the running instance and save it
  --mock           Start in the background with simulated (demo) sensor data
  --version        Print the version
";

fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn send(req: Request) -> Result<Response, String> {
    let mut c = ipc::Client::connect().map_err(|e| format!("FreeHM is not running ({e})"))?;
    c.call(&req).map_err(|e| e.to_string())
}

/// Same path as the settings window: the running instance validates, applies and saves.
fn apply_config(file: Option<&str>) -> i32 {
    let Some(file) = file else {
        eprintln!("usage: freehm --apply-config FILE");
        return 2;
    };
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read {file}: {e}");
            return 1;
        }
    };
    let (cfg, notes) = match freehm_core::config::Config::from_json(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{file} is not a valid configuration: {e}");
            return 1;
        }
    };
    // Loading fixes out-of-range values silently; a command asked to apply exactly this
    // file refuses instead (the same rule as `Config::validate`).
    let problems: Vec<String> =
        notes.into_iter().filter(|n| !n.contains("missing from configuration") && !n.contains("migrated") && !n.contains("newer FreeHM")).collect();
    if !problems.is_empty() {
        for p in problems {
            eprintln!("rejected: {p}");
        }
        return 1;
    }
    match send(Request::ApplyConfig(Box::new(cfg))) {
        Ok(Response::Ok) => 0,
        Ok(Response::Rejected(problems)) => {
            for p in problems {
                eprintln!("rejected: {p}");
            }
            1
        }
        Ok(Response::Error(e)) => {
            eprintln!("error: {e}");
            1
        }
        Ok(_) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let code = match cmd {
        "--help" | "-h" => {
            attach_console();
            print!("{HELP}");
            0
        }
        "--version" | "-V" => {
            attach_console();
            println!("FreeHM {}", freehm_core::APP_VERSION);
            0
        }
        "--settings" => {
            logging::init(&paths::log_dir().join("settings.log"), false);
            settings::run()
        }
        "--toggle" | "--show" | "--hide" | "--quit" | "--status" => {
            attach_console();
            let req = match cmd {
                "--toggle" => Request::ToggleOverlay,
                "--show" => Request::SetOverlayVisible(true),
                "--hide" => Request::SetOverlayVisible(false),
                "--quit" => Request::Quit,
                _ => Request::GetStatus { live: false, show_overlay: false },
            };
            match send(req) {
                Ok(Response::Status(s)) => {
                    println!("{}", serde_json::to_string_pretty(&*s).unwrap_or_default());
                    0
                }
                Ok(Response::Error(e)) => {
                    eprintln!("error: {e}");
                    1
                }
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        "--apply-config" => {
            attach_console();
            apply_config(args.get(1).map(String::as_str))
        }
        #[cfg(target_os = "linux")]
        "--setup-permissions" | "--remove-permissions" => {
            attach_console();
            platform::linux::permissions::cli(cmd, &args[1..])
        }
        "" | "--background" | "--mock" => run_background(cmd),
        other => {
            attach_console();
            eprintln!("unknown command '{other}'\n\n{HELP}");
            2
        }
    };
    std::process::exit(code);
}

fn run_background(cmd: &str) -> i32 {
    // Already running? Forward the request instead of starting a second instance.
    if let Ok(mut c) = ipc::Client::connect() {
        if cmd.is_empty() {
            let _ = c.call(&Request::OpenSettings);
        }
        return 0;
    }
    let Some(_guard) = platform::single_instance() else {
        // Another instance is starting up right now.
        return 0;
    };
    logging::init(&paths::log_file(), std::env::var_os("FREEHM_LOG_STDERR").is_some());
    platform::set_dpi_awareness();
    let path = paths::config_file();
    let loaded = Config::load(&path);
    for n in &loaded.notes {
        log::info!("config: {n}");
    }
    let mut cfg = loaded.config;
    let mut notes = loaded.notes;
    let first_run = !cfg.permissions.first_run_done;
    if first_run {
        cfg.permissions.first_run_done = true;
        if let Err(e) = cfg.save(&path) {
            notes.push(format!("could not save configuration: {e}"));
        }
    }
    platform::run(cfg, path, notes, cmd == "--mock", first_run && cmd.is_empty())
}
