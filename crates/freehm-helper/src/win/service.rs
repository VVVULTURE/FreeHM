//! Service Control Manager integration.

use super::server::Server;
use freehm_core::helper_proto::SERVICE_NAME;
use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;
use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

define_windows_service!(ffi_service_main, service_main);

pub fn dispatch() -> Result<(), String> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| format!("service dispatcher: {e}"))
}

fn status(state: ServiceState, exit: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(exit),
        checkpoint: 0,
        wait_hint: if state == ServiceState::StopPending { Duration::from_secs(5) } else { Duration::ZERO },
        process_id: None,
    }
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run() {
        log::error!("helper service failed: {e}");
    }
}

fn run() -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<()>();
    let handler = move |ev| match ev {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let h = service_control_handler::register(SERVICE_NAME, handler).map_err(|e| e.to_string())?;
    let mut server = match Server::start() {
        Ok(s) => s,
        Err(e) => {
            // ERROR_SERVICE_SPECIFIC_ERROR-style exit so the SCM records a failure.
            let _ = h.set_service_status(status(ServiceState::Stopped, 1));
            return Err(e);
        }
    };
    h.set_service_status(status(ServiceState::Running, 0)).map_err(|e| e.to_string())?;
    let _ = rx.recv();
    let _ = h.set_service_status(status(ServiceState::StopPending, 0));
    server.shutdown();
    h.set_service_status(status(ServiceState::Stopped, 0)).map_err(|e| e.to_string())?;
    Ok(())
}
