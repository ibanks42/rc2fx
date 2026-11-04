//! Device-side orchestration. Only the explicit audio endpoint runs in callbacks;
//! The control API, storage, state preparation and reclamation belong to control threads.
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod api;
pub mod audio;
pub mod catalog;
pub mod controller;
pub mod coordinator;
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod developer_ssh;
pub mod diagnostic_history;
pub mod diagnostics;
pub mod diagnostics_log;
pub mod events;
mod facade;
pub mod metrics;
#[cfg(all(feature = "native-pipeline", target_os = "linux"))]
pub mod native_backend;
pub mod native_plan;
mod overload;
mod persistence;
pub mod stock_inputs;
pub mod show_watch;
pub mod store;
#[cfg(target_os = "linux")]
pub mod ui_control;
mod usb;
pub mod versions;
#[cfg(any(target_os = "linux", test))]
pub mod usb_control;
