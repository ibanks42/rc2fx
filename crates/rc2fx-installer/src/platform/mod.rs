//! Operating-system USB access: HID control interfaces and update volumes.

use crate::Result;
use serde::Serialize;
use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "macos", test))]
pub(crate) mod macdisk;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Hid {
    pub path: String,
    pub serial: String,
    pub pid: u16,
    /// HID usage page of the top-level collection.
    pub usage: u16,
    pub input: usize,
    pub output: usize,
    /// The descriptor declares RC2FX's control reports: RC2FX's USB layout
    /// is live on the mixer.
    pub control: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Volume {
    pub path: String,
    pub device: String,
    pub serial: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
}

/// An open HID report channel. Closed on drop.
pub trait Reports {
    fn write(&mut self, report: &[u8], timeout: Duration) -> Result<usize>;
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize>;
}

pub trait Platform: Send {
    fn inventory(&self) -> Result<Vec<Hid>>;
    fn open(&self, hid: &Hid) -> Result<Box<dyn Reports>>;
    /// Ordinary HID control does not require update-volume administration.
    fn open_control(&self, hid: &Hid) -> Result<Box<dyn Reports>> {
        self.open(hid)
    }
    fn volumes(&self) -> Result<Vec<Volume>>;
    fn mount(&self, volume: Volume) -> Result<Volume>;
    fn media_present(&self, volume: &Volume) -> Result<bool>;
    fn eject(&self, volume: &Volume) -> Result<()>;
}

pub fn host() -> Box<dyn Platform> {
    #[cfg(windows)]
    return Box::new(windows::Host);
    #[cfg(target_os = "linux")]
    return Box::new(linux::Host);
    #[cfg(target_os = "macos")]
    return Box::new(macos::Host);
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    return Box::new(Unsupported);
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
struct Unsupported;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
impl Platform for Unsupported {
    fn inventory(&self) -> Result<Vec<Hid>> {
        Err("USB installation is not supported on this system".into())
    }
    fn open(&self, _: &Hid) -> Result<Box<dyn Reports>> {
        Err("USB installation is not supported on this system".into())
    }
    fn volumes(&self) -> Result<Vec<Volume>> {
        Err("USB installation is not supported on this system".into())
    }
    fn mount(&self, _: Volume) -> Result<Volume> {
        Err("USB installation is not supported on this system".into())
    }
    fn media_present(&self, _: &Volume) -> Result<bool> {
        Err("USB installation is not supported on this system".into())
    }
    fn eject(&self, _: &Volume) -> Result<()> {
        Err("USB installation is not supported on this system".into())
    }
}
