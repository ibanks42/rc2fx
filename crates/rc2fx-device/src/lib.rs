//! Device-side control: the control API, storage, state preparation and the
//! connection to the native engine. Nothing here runs in an audio callback;
//! the engine runs in the mixer process (`rc2fx-native`).
#[cfg(not(target_os = "linux"))]
compile_error!("rc2fx's mixer crates build for Linux only");

pub mod control;
pub mod diagnostics;
pub mod engine;
pub mod mixer_update;
pub mod storage;
pub mod transport;

pub(crate) use control::facade;
pub use control::{api, controller, coordinator, developer_ssh, ui_control};
pub use diagnostics::{diagnostic_history, diagnostics_log, events, recent};
pub(crate) use engine::proc_stat;
pub use engine::{catalog, native_backend, native_plan, show_watch, stock_inputs, versions};
pub(crate) use storage::persistence;
pub use storage::store;
pub(crate) use transport::usb;
pub use transport::usb_control;

#[cfg(test)]
pub(crate) fn socket_tempdir(leaf: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // Linux sockaddr_un.sun_path holds at most 108 bytes including NUL,
    // and rc2fx-native enforces <= 103 bytes.
    if dir.path().join(leaf).as_os_str().as_encoded_bytes().len() <= 103 {
        return dir;
    }
    tempfile::Builder::new()
        .prefix("rc2fx-")
        .tempdir_in("/tmp")
        .unwrap()
}
