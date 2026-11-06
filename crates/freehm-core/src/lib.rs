//! Platform-neutral core of FreeHM.
//!
//! Nothing in this crate touches the operating system beyond reading/writing files, so all
//! of it is unit-tested on every CI platform. Platform code lives in the `freehm` crate.

pub mod config;
pub mod format;
pub mod frames;
pub mod gamedetect;
pub mod helper_proto;
pub mod hotkey;
pub mod ipc;
pub mod metrics;
pub mod policy;
pub mod render;
pub mod telemetry;
pub mod update;
pub mod wire;

pub const APP_NAME: &str = "FreeHM";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
