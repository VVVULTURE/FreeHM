//! Protocol between the unprivileged app and the privileged Windows helper service.
//!
//! The helper exposes exactly one operation — "give me the latest sensor snapshot" — with
//! two boolean switches. There is no way to name an MSR, a file, a process to open or a
//! command to run, so the pipe cannot be used for anything but reading these values.

use crate::frames::ProcessFrames;
use serde::{Deserialize, Serialize};

pub const PIPE_NAME: &str = r"\\.\pipe\FreeHM.Helper.v1";
pub const SERVICE_NAME: &str = "FreeHMHelper";
pub const PROTOCOL_VERSION: u32 = 1;
/// Requests are tiny; anything larger is rejected before parsing.
pub const MAX_REQUEST: usize = 256;
pub const MAX_RESPONSE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperRequest {
    pub v: u32,
    /// Read CPU temperature/power via PawnIO.
    pub cpu: bool,
    /// Collect present statistics via ETW. The helper starts its ETW session on the first
    /// request with `frames: true` and stops it after a period without such requests.
    pub frames: bool,
}

impl HelperRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.v != PROTOCOL_VERSION {
            return Err(format!("unsupported protocol version {} (helper speaks {PROTOCOL_VERSION})", self.v));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HelperResponse {
    pub v: u32,
    pub helper_version: String,
    pub error: Option<String>,
    pub cpu: Option<CpuSensors>,
    pub frames: Option<FrameReport>,
}

/// A value or the reason it is missing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value<T> {
    Ok(T),
    Unavailable(String),
}

impl<T: Copy> Value<T> {
    pub fn ok(&self) -> Option<T> {
        match self {
            Value::Ok(v) => Some(*v),
            Value::Unavailable(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuSensors {
    /// Package temperature (Intel: IA32_PACKAGE_THERM_STATUS; AMD: Tctl/Tdie via SMN).
    pub package_temp_c: Value<f64>,
    /// Where the temperature came from, e.g. "Intel package DTS" or "AMD Tctl".
    pub temp_source: String,
    /// Package power from RAPL energy counters (fallback when the OS energy meter is absent).
    pub package_power_w: Value<f64>,
    pub cpu_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameReport {
    /// False when the ETW session could not be started; `reason` says why.
    pub available: bool,
    pub reason: Option<String>,
    pub processes: Vec<ProcessFrames>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_fields_and_versions() {
        let ok: HelperRequest = serde_json::from_str(r#"{"v":1,"cpu":true,"frames":false}"#).unwrap();
        assert!(ok.validate().is_ok());
        assert!(serde_json::from_str::<HelperRequest>(r#"{"v":1,"cpu":true,"frames":false,"msr":409}"#).is_err());
        let old: HelperRequest = serde_json::from_str(r#"{"v":7,"cpu":true,"frames":true}"#).unwrap();
        assert!(old.validate().is_err());
    }

    #[test]
    fn value_accessor() {
        assert_eq!(Value::Ok(3.5).ok(), Some(3.5));
        assert_eq!(Value::<f64>::Unavailable("x".into()).ok(), None);
    }
}
