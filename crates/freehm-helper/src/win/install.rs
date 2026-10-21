//! Install, repair and remove the helper service (run elevated, once).
//!
//! A LocalSystem service must run a binary that unprivileged users cannot replace, so the
//! helper only registers itself from a directory under Program Files. When run from
//! anywhere else (a portable copy, a build folder) it first copies itself to
//! `%ProgramFiles%\FreeHM\Helper`, whose ACL is inherited from Program Files.

use super::logfile::CliOutput;
use freehm_core::helper_proto::SERVICE_NAME;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::System::Services::{ChangeServiceConfig2W, SC_HANDLE, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, SERVICE_REQUIRED_PRIVILEGES_INFOW};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType,
    ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const DISPLAY_NAME: &str = "FreeHM Hardware Helper";
const DESCRIPTION: &str = "Provides CPU temperature (via the PawnIO driver) and game frame timing (via Windows graphics event tracing) to the FreeHM overlay. Read-only; serves local users only.";

fn program_files() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramW6432").or_else(|| std::env::var_os("ProgramFiles")).unwrap_or_else(|| r"C:\Program Files".into()))
}

fn helper_dir() -> PathBuf {
    program_files().join("FreeHM").join("Helper")
}

fn under(path: &Path, dir: &Path) -> bool {
    match (path.canonicalize(), dir.canonicalize()) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => false,
    }
}

fn wait_state(svc: &windows_service::service::Service, want: ServiceState, timeout: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < timeout {
        if svc.query_status().map(|s| s.current_state == want).unwrap_or(false) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Keep only the privileges the service needs; everything else is removed from its token.
fn restrict_privileges(svc: &windows_service::service::Service) -> Result<(), String> {
    let list = "SeChangeNotifyPrivilege\0SeCreateGlobalPrivilege\0\0";
    let mut w: Vec<u16> = list.encode_utf16().collect();
    let info = SERVICE_REQUIRED_PRIVILEGES_INFOW { pmszRequiredPrivileges: windows::core::PWSTR(w.as_mut_ptr()) };
    unsafe {
        ChangeServiceConfig2W(SC_HANDLE(svc.raw_handle() as _), SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, Some(&info as *const _ as *const _))
            .map_err(|e| format!("could not restrict service privileges: {e}"))
    }
}

pub fn install(out: &CliOutput) -> Result<(), String> {
    let mgr = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .map_err(|e| format!("cannot open the Service Control Manager ({e}); administrator rights are required"))?;
    let me = std::env::current_exe().map_err(|e| e.to_string())?;

    // Stop an existing instance first so its binary can be replaced.
    let existing = mgr.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG | ServiceAccess::START).ok();
    if let Some(svc) = &existing
        && svc.query_status().map(|s| s.current_state != ServiceState::Stopped).unwrap_or(false)
    {
        out.line("stopping the running helper…");
        let _ = svc.stop();
        wait_state(svc, ServiceState::Stopped, Duration::from_secs(15));
    }

    let exe = if under(&me, &program_files()) {
        me.clone()
    } else {
        let dir = helper_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let target = dir.join("freehm-helper.exe");
        let tmp = dir.join("freehm-helper.exe.new");
        std::fs::copy(&me, &tmp).map_err(|e| format!("cannot copy the helper to {}: {e}", dir.display()))?;
        std::fs::rename(&tmp, &target).map_err(|e| format!("cannot replace {}: {e}", target.display()))?;
        out.line(&format!("copied helper to {}", target.display()));
        target
    };

    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.clone(),
        launch_arguments: vec![OsString::from("service")],
        dependencies: vec![],
        account_name: None, // LocalSystem: required to open the PawnIO device and run ETW sessions
        account_password: None,
    };
    let svc = match existing {
        Some(svc) => {
            svc.change_config(&info).map_err(|e| format!("cannot update the service: {e}"))?;
            out.line("updated the existing service");
            svc
        }
        None => {
            let svc = mgr
                .create_service(&info, ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG)
                .map_err(|e| format!("cannot create the service: {e}"))?;
            out.line("created service FreeHMHelper");
            svc
        }
    };
    let _ = svc.set_description(DESCRIPTION);
    restrict_privileges(&svc)?;
    let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(10) };
    let _ = svc.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart, ServiceAction { action_type: ServiceActionType::None, delay: Duration::ZERO }]),
    });
    svc.start(&[] as &[&str]).map_err(|e| format!("the service was installed but did not start: {e}"))?;
    if !wait_state(&svc, ServiceState::Running, Duration::from_secs(15)) {
        return Err("the service was installed but did not reach the running state; see %ProgramData%\\FreeHM\\helper.log".into());
    }
    out.line("helper installed and running");
    Ok(())
}

pub fn uninstall(out: &CliOutput) -> Result<(), String> {
    let mgr = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("cannot open the Service Control Manager ({e}); administrator rights are required"))?;
    match mgr.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_CONFIG) {
        Ok(svc) => {
            let path = svc.query_config().ok().map(|c| c.executable_path);
            if svc.query_status().map(|s| s.current_state != ServiceState::Stopped).unwrap_or(false) {
                let _ = svc.stop();
                wait_state(&svc, ServiceState::Stopped, Duration::from_secs(15));
            }
            svc.delete().map_err(|e| format!("cannot delete the service: {e}"))?;
            out.line("removed service FreeHMHelper");
            // Remove the copy this tool made (never files of an installer-managed folder).
            if let Some(p) = path {
                let p = PathBuf::from(p.to_string_lossy().trim_matches('"').to_string());
                if under(&p, &helper_dir()) {
                    let me = std::env::current_exe().ok();
                    if me.as_deref().is_some_and(|m| under(m, &helper_dir())) {
                        out.line("note: run uninstall from the FreeHM folder to delete the helper copy");
                    } else {
                        let _ = std::fs::remove_file(&p);
                        let _ = std::fs::remove_dir(helper_dir());
                        let _ = std::fs::remove_dir(helper_dir().parent().unwrap());
                    }
                }
            }
        }
        Err(_) => out.line("the helper service is not installed"),
    }
    Ok(())
}

pub fn status() -> String {
    let Ok(mgr) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else {
        return "cannot query the Service Control Manager".into();
    };
    match mgr.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(svc) => match svc.query_status() {
            Ok(s) => format!("installed, state {:?}", s.current_state),
            Err(e) => format!("installed, state unknown ({e})"),
        },
        Err(_) => "not installed".into(),
    }
}
