//! Device-side control: the control API, storage, state preparation and the
//! connection to the native engine. Nothing here runs in an audio callback;
//! the engine runs in the mixer process (rc2fx-native).
#[cfg(not(target_os = "linux"))]
compile_error!("rc2fx's mixer crates build for Linux only");
pub mod api;
pub mod catalog;
pub mod controller;
pub mod coordinator;
pub mod developer_ssh;
pub mod diagnostic_history;
pub mod diagnostics_log;
pub mod events;
mod facade;
pub mod mixer_update;
pub mod native_backend;
pub mod native_plan;
mod persistence;
mod proc_stat;
pub mod recent;
pub mod show_watch;
pub mod stock_inputs;
pub mod store;
pub mod ui_control;
mod usb;
pub mod usb_control;
pub mod versions;
