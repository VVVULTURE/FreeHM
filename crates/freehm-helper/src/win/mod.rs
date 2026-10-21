mod cpu;
mod etw;
mod install;
mod logfile;
mod pawnio;
mod sd;
mod server;
mod service;

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let log_arg = args.iter().position(|a| a == "--log").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
    match cmd {
        "service" => {
            logfile::init_service_log();
            match service::dispatch() {
                Ok(()) => 0,
                Err(e) => {
                    log::error!("{e}");
                    eprintln!("{e}\n(this command is used by the Service Control Manager; use `install` to set the service up)");
                    1
                }
            }
        }
        "install" | "uninstall" => {
            let out = logfile::CliOutput::new(log_arg);
            let r = if cmd == "install" { install::install(&out) } else { install::uninstall(&out) };
            match r {
                Ok(()) => 0,
                Err(e) => {
                    out.line(&format!("error: {e}"));
                    1
                }
            }
        }
        "status" => {
            let st = install::status();
            println!("{st}");
            0
        }
        "selftest" => server::selftest(),
        _ => {
            eprintln!("FreeHM helper {}\nusage: freehm-helper install|uninstall [--log FILE] | status | selftest", freehm_core::APP_VERSION);
            2
        }
    }
}
