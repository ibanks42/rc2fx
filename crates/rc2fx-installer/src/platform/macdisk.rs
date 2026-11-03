//! macOS update-disk identification from `ioreg` and `diskutil` property
//! lists. Pure parsing, so it is tested on every platform.

use crate::usb::{VENDOR_ID, is_update_pid};
use plist::Value;
use std::collections::BTreeMap;

pub(crate) fn valid_disk_name(n: &str) -> bool {
    n.len() > 4 && n.starts_with("disk") && n[4..].bytes().all(|c| c.is_ascii_digit() || c == b's')
}

fn integer(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_signed_integer)
}

/// Map `/dev/diskN[sM]` to the serial of the Pro II in update mode above it,
/// from `ioreg -a -r -c IOUSBHostDevice`.
pub(crate) fn usb_disks(tree: &Value) -> BTreeMap<String, String> {
    fn walk(node: &Value, serial: &str, out: &mut BTreeMap<String, String>) {
        match node {
            Value::Array(items) => items.iter().for_each(|v| walk(v, serial, out)),
            Value::Dictionary(d) => {
                let mut serial = serial.to_owned();
                if d.contains_key("idVendor") {
                    serial.clear();
                    if integer(d.get("idVendor")) == Some(VENDOR_ID.into())
                        && integer(d.get("idProduct"))
                            .and_then(|p| u16::try_from(p).ok())
                            .is_some_and(is_update_pid)
                    {
                        serial = d
                            .get("USB Serial Number")
                            .and_then(Value::as_string)
                            .unwrap_or_default()
                            .to_owned();
                    }
                }
                if !serial.is_empty()
                    && let Some(name) = d.get("BSD Name").and_then(Value::as_string)
                    && valid_disk_name(name)
                {
                    out.insert(format!("/dev/{name}"), serial.clone());
                }
                if let Some(children) = d.get("IORegistryEntryChildren") {
                    walk(children, &serial, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    walk(tree, "", &mut out);
    out
}

/// Mount point, FAT detection and capacity from `diskutil info -plist`.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct DiskInfo {
    pub mount_point: String,
    pub fat: bool,
    pub total: u64,
    pub free: u64,
}

pub(crate) fn disk_info(info: &Value) -> Option<DiskInfo> {
    let d = info.as_dictionary()?;
    let text = |k: &str| d.get(k).and_then(Value::as_string).unwrap_or_default();
    Some(DiskInfo {
        mount_point: text("MountPoint").to_owned(),
        fat: text("FilesystemType") == "msdos",
        total: integer(d.get("TotalSize")).filter(|n| *n > 0).unwrap_or(0) as u64,
        free: integer(d.get("VolumeFreeSpace"))
            .filter(|n| *n >= 0)
            .unwrap_or(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_update_disks_and_disk_info() {
        let tree = Value::from_reader_xml(
            r#"<?xml version="1.0"?><plist><array><dict>
            <key>idVendor</key><integer>6647</integer><key>idProduct</key><integer>48</integer>
            <key>USB Serial Number</key><string>GV0044100</string>
            <key>IORegistryEntryChildren</key><array>
              <dict><key>BSD Name</key><string>disk4</string></dict>
              <dict><key>BSD Name</key><string>disk4s1</string></dict>
              <dict><key>BSD Name</key><string>../evil</string></dict>
              <dict><key>idVendor</key><integer>1</integer><key>IORegistryEntryChildren</key><array>
                <dict><key>BSD Name</key><string>disk9</string></dict></array></dict>
            </array></dict></array></plist>"#
                .as_bytes(),
        )
        .unwrap();
        let disks = usb_disks(&tree);
        for pid in [79, 80, 38] {
            // Duo update, Duo normal, USB 2
            let mut other = tree.clone();
            other.as_array_mut().unwrap()[0]
                .as_dictionary_mut()
                .unwrap()
                .insert("idProduct".into(), Value::Integer(pid.into()));
            assert_eq!(usb_disks(&other).len(), if pid == 79 { 2 } else { 0 });
        }
        assert_eq!(disks.len(), 2);
        assert_eq!(disks["/dev/disk4s1"], "GV0044100");
        let info = Value::from_reader_xml(
            r#"<plist><dict><key>MountPoint</key><string>/Volumes/Test &amp; Space</string>
            <key>FilesystemType</key><string>msdos</string>
            <key>TotalSize</key><integer>283189248</integer></dict></plist>"#
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            disk_info(&info),
            Some(DiskInfo {
                mount_point: "/Volumes/Test & Space".into(),
                fat: true,
                total: 283_189_248,
                free: 0
            })
        );
        assert!(valid_disk_name("disk12s3") && !valid_disk_name("disk") && !valid_disk_name("sda"));
    }
}
