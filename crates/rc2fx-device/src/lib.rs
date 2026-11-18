//! Device-side control: the control API, storage, state preparation and the
//! connection to the native engine. Nothing here runs in an audio callback;
//! the engine runs in the mixer process (rc2fx-native).
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod api;
pub mod catalog;
pub mod controller;
pub mod coordinator;
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod developer_ssh;
pub mod diagnostic_history;
pub mod diagnostics_log;
pub mod events;
mod facade;
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod mixer_update;
#[cfg(all(feature = "native-pipeline", target_os = "linux"))]
pub mod native_backend;
pub mod native_plan;
mod persistence;
pub mod recent;
pub mod show_watch;
pub mod stock_inputs;
pub mod store;
#[cfg(target_os = "linux")]
pub mod ui_control;
mod usb;
#[cfg(any(target_os = "linux", test))]
pub mod usb_control;
pub mod versions;
