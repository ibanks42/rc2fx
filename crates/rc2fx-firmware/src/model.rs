//! Board identity is independent of the mixer executable: Pro II and Duo
//! 1.7.6 ship the same executable but different board/controller firmware.
use crate::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Model {
    #[default]
    #[serde(rename = "rodecaster-pro-ii")]
    ProIi,
    #[serde(rename = "rodecaster-duo")]
    Duo,
}

impl Model {
    pub const ALL: [Self; 2] = [Self::ProIi, Self::Duo];
    pub const VENDOR_ID: u16 = 0x19f7;

    pub fn parse(id: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|m| m.id() == id)
            .ok_or_else(|| format!("unknown mixer model {id}"))
    }
    pub const fn id(self) -> &'static str {
        match self {
            Self::ProIi => "rodecaster-pro-ii",
            Self::Duo => "rodecaster-duo",
        }
    }
    pub const fn hostname(self) -> &'static str {
        match self {
            Self::ProIi => "rodecaster-pro-ii",
            Self::Duo => "rodecaster-pro-ii-mini",
        }
    }
    pub const fn board_dtb(self) -> &'static str {
        match self {
            Self::ProIi => "/boot/meson-axg-rodecaster-pro-ii.dtb",
            Self::Duo => "/boot/meson-axg-rodecaster-pro-ii-mini.dtb",
        }
    }
    pub const fn firmware_directory(self) -> &'static str {
        match self {
            Self::ProIi => "rcp2",
            Self::Duo => "rcp2-duo",
        }
    }
    /// Models admitted without an explicit experimental opt-in.
    pub const fn runtime_enabled(self) -> bool {
        matches!(self, Self::ProIi)
    }
    pub fn require_runtime(self) -> Result<()> {
        self.require_runtime_with(false)
    }
    pub fn require_runtime_with(self, experimental_duo: bool) -> Result<()> {
        if self.runtime_enabled() || (self == Self::Duo && experimental_duo) {
            Ok(())
        } else {
            Err("RØDECaster Duo support is experimental and untested on hardware. Enable experimental Duo flashing at your own risk.".into())
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::ProIi => "RØDECaster Pro II",
            Self::Duo => "RØDECaster Duo",
        }
    }
    pub const fn route_layout(self) -> &'static str {
        match self {
            Self::ProIi => "pro-ii-25-v1",
            Self::Duo => "duo-23-v1",
        }
    }
    pub const fn source_enum(self) -> &'static str {
        match self {
            Self::ProIi => "pro-ii-sources-v1",
            Self::Duo => "duo-sources-v1",
        }
    }
    pub const fn normal_pids(self) -> &'static [u16; 5] {
        match self {
            Self::ProIi => &[0x0037, 0x0078, 0x008d, 0x0092, 0x0094],
            Self::Duo => &[0x0050, 0x0079, 0x008e, 0x0093, 0x0095],
        }
    }
    pub const fn update_pid(self) -> u16 {
        match self {
            Self::ProIi => 0x0030,
            Self::Duo => 0x004f,
        }
    }
    pub fn from_usb(vendor: u16, product: u16) -> Option<Self> {
        (vendor == Self::VENDOR_ID)
            .then(|| {
                Self::ALL
                    .into_iter()
                    .find(|m| m.normal_pids().contains(&product) || m.update_pid() == product)
            })
            .flatten()
    }
    /// Require consistent rootfs and boot-board evidence. No environment
    /// override or executable hash may impersonate a different device model.
    pub fn from_board(hostname: &str, extlinux: &str) -> Result<Self> {
        let mut boards = extlinux.lines().filter_map(|line| {
            let mut words = line.split_whitespace();
            match words.next() {
                Some(key) if key.eq_ignore_ascii_case("FDT") => Some(words.collect::<Vec<_>>()),
                _ => None,
            }
        });
        let board = boards.next().ok_or("missing boot board identity")?;
        if boards.next().is_some() || board.len() != 1 {
            return Err("ambiguous boot board identity".into());
        }
        Self::ALL
            .into_iter()
            .find(|m| m.hostname() == hostname.trim() && m.board_dtb() == board[0])
            .ok_or_else(|| "unknown or conflicting hostname and boot board identity".into())
    }
    pub fn read_device() -> Result<Self> {
        Self::read_root(Path::new("/"))
    }
    pub fn read_root(root: &Path) -> Result<Self> {
        let read = |path: &str| {
            std::fs::read_to_string(root.join(path)).map_err(|e| format!("{path}: {e}"))
        };
        Self::from_board(
            &read("etc/hostname")?,
            &read("boot/extlinux/extlinux.conf")?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usb_modes_are_model_specific_and_secondary_usb_is_not_control() {
        for model in Model::ALL {
            for pid in model
                .normal_pids()
                .iter()
                .chain(std::iter::once(&model.update_pid()))
            {
                assert_eq!(Model::from_usb(Model::VENDOR_ID, *pid), Some(model));
                assert_eq!(Model::from_usb(0, *pid), None);
            }
        }
        assert_eq!(Model::from_usb(Model::VENDOR_ID, 0x0026), None);
        assert_eq!(Model::from_usb(Model::VENDOR_ID, 0xffff), None);
        assert!(Model::Duo.require_runtime().is_err());
    }
    #[test]
    fn board_evidence_must_agree_and_be_unambiguous() {
        for model in Model::ALL {
            let boot = format!("LABEL primary\n  FDT {}\n", model.board_dtb());
            assert_eq!(Model::from_board(model.hostname(), &boot).unwrap(), model);
            let other = if model == Model::Duo {
                Model::ProIi
            } else {
                Model::Duo
            };
            assert!(Model::from_board(other.hostname(), &boot).is_err());
            assert!(Model::from_board(model.hostname(), &format!("{boot}{boot}")).is_err());
            assert!(Model::from_board(model.hostname(), "# FDT ignored").is_err());
        }
    }
}
