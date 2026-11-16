//! A small fake mixer disk built from real parts: an MBR with RØDE's four
//! partitions, two ext4 roots made by `mkfs.ext4` with a Pro II's features,
//! and a FAT settings partition made by `mkfs.vfat`. Skipped where those
//! tools are missing; CI's Ubuntu has them.
use super::*;
use crate::platform::{Hid, Platform, RecoveryDevice, Reports, Volume};
use std::process::Command;

const SCRIPT: &[u8] = b"#!/bin/sh\nmount -t vfat /dev/mmcblk1p3 /Application/update-staging\n";
const MIB: u64 = 1 << 20;
/// Partition starts and sizes in MiB.
const PARTS: [(u64, u64); 4] = [(1, 8), (9, 8), (17, 34), (51, 1)];

fn tool(name: &str) -> Option<PathBuf> {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.exists())
}

struct Disk {
    dir: tempfile::TempDir,
    path: PathBuf,
}

struct Tree {
    hostname: &'static str,
    manifest: bool,
}

impl Disk {
    fn new(trees: [Tree; 2]) -> Option<Self> {
        let (Some(mkfs), Some(vfat)) = (tool("mkfs.ext4"), tool("mkfs.vfat")) else {
            eprintln!("skipped: mkfs.ext4 or mkfs.vfat is not installed");
            return None;
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.img");
        let disk = std::fs::File::create(&path).unwrap();
        disk.set_len(52 * MIB).unwrap();
        drop(disk);
        let mut sector0 = [0u8; 512];
        for (i, (start, len)) in PARTS.iter().enumerate() {
            let entry = &mut sector0[446 + 16 * i..462 + 16 * i];
            entry[4] = if i < 2 { 0x83 } else { 0x0c };
            entry[8..12].copy_from_slice(&((start * MIB / SECTOR) as u32).to_le_bytes());
            entry[12..16].copy_from_slice(&((len * MIB / SECTOR) as u32).to_le_bytes());
        }
        sector0[510] = 0x55;
        sector0[511] = 0xAA;
        write_at(&path, 0, &sector0);
        for (i, tree) in trees.iter().enumerate() {
            let root = dir.path().join(format!("tree{i}"));
            for sub in ["usr/bin", "etc", "opt/rc2fx"] {
                std::fs::create_dir_all(root.join(sub)).unwrap();
            }
            std::fs::write(root.join("etc/hostname"), format!("{}\n", tree.hostname)).unwrap();
            std::fs::write(root.join("usr/bin/rode-init-update-env.sh"), SCRIPT).unwrap();
            std::fs::write(root.join("usr/bin/big"), big()).unwrap();
            std::fs::write(root.join("opt/rc2fx/SHA256SUMS"), b"aa  rc2fxd\n").unwrap();
            if tree.manifest {
                std::fs::write(
                    root.join(image_check::MANIFEST_MEMBER),
                    manifest(tree.hostname),
                )
                .unwrap();
            }
            let image = dir.path().join(format!("root{i}.img"));
            let block = if i == 0 { "1024" } else { "4096" };
            let status = Command::new(&mkfs)
                .args(["-F", "-q", "-t", "ext4", "-b", block, "-I", "256", "-O"])
                .arg("metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file,^metadata_csum_seed")
                .arg("-d")
                .arg(&root)
                .arg(&image)
                .arg(format!("{}M", PARTS[i].1))
                .status()
                .unwrap();
            assert!(status.success());
            write_at(&path, PARTS[i].0 * MIB, &std::fs::read(&image).unwrap());
        }
        let fat = dir.path().join("fat.img");
        let status = Command::new(&vfat)
            .args(["-F", "32", "-C"])
            .arg(&fat)
            .arg((PARTS[2].1 * 1024).to_string())
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        write_at(&path, PARTS[2].0 * MIB, &std::fs::read(&fat).unwrap());
        Some(Self { dir, path })
    }

    fn engine(&self) -> Engine {
        let work = self.dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        Engine::with_platform(self.dir.path(), work, Box::new(FakeDisk(self.path.clone())))
    }

    fn root(&self, index: usize, writable: bool) -> Filesystem {
        Filesystem::open(&self.path, PARTS[index].0 * MIB, writable).unwrap()
    }

    /// What the bad flash did: the right size, no contents.
    fn zero(&self, index: usize, path: &str) {
        let mut fs = self.root(index, true);
        let entry = fs.lookup(path).unwrap().unwrap();
        fs.overwrite(&entry, &vec![0; entry.size as usize]).unwrap();
        fs.close().unwrap();
    }

    fn contents(&self, index: usize, path: &str) -> Vec<u8> {
        let fs = self.root(index, false);
        let entry = fs.lookup(path).unwrap().unwrap();
        fs.read(&entry).unwrap()
    }

    /// `e2fsck -fn` on one root, copied out of the disk.
    fn clean(&self, index: usize) {
        let Some(e2fsck) = tool("e2fsck") else { return };
        let bytes = std::fs::read(&self.path).unwrap();
        let (start, len) = (PARTS[index].0 * MIB, PARTS[index].1 * MIB);
        let image = self.dir.path().join("check.img");
        std::fs::write(&image, &bytes[start as usize..(start + len) as usize]).unwrap();
        let out = Command::new(e2fsck)
            .args(["-f", "-n"])
            .arg(&image)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

fn big() -> Vec<u8> {
    (0..200_000u32).map(|n| (n * 13 % 251) as u8).collect()
}

fn manifest(hostname: &str) -> String {
    [
        (
            files::sum(format!("{hostname}\n").as_bytes()),
            "etc/hostname",
        ),
        (files::sum(SCRIPT), "usr/bin/rode-init-update-env.sh"),
        (files::sum(&big()), "usr/bin/big"),
    ]
    .iter()
    .map(|(sha, path)| image_check::line(sha, path))
    .collect()
}

fn write_at(path: &Path, offset: u64, data: &[u8]) {
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(data).unwrap();
}

fn target(root: u8, path: &str) -> RepairTarget {
    RepairTarget {
        root,
        path: path.into(),
    }
}

const PRO_II: Tree = Tree {
    hostname: "rodecaster-pro-ii",
    manifest: true,
};

struct FakeDisk(PathBuf);

impl Platform for FakeDisk {
    fn inventory(&self) -> Result<Vec<Hid>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &Hid) -> Result<Box<dyn Reports>> {
        bail!("no HID")
    }
    fn volumes(&self) -> Result<Vec<Volume>> {
        Ok(Vec::new())
    }
    fn mount(&self, _: Volume) -> Result<Volume> {
        bail!("no volumes")
    }
    fn media_present(&self, _: &Volume) -> Result<bool> {
        Ok(false)
    }
    fn eject(&self, _: &Volume) -> Result<()> {
        Ok(())
    }
    fn recovery_disk(&self) -> Result<Option<u64>> {
        Ok(Some(std::fs::metadata(&self.0)?.len()))
    }
    fn recovery_device(&self) -> Result<RecoveryDevice> {
        Ok(RecoveryDevice {
            path: self.0.clone(),
            len: std::fs::metadata(&self.0)?.len(),
            _claim: None,
        })
    }
}

#[test]
fn a_healthy_mixer_has_nothing_to_repair() {
    let Some(disk) = Disk::new([PRO_II, PRO_II]) else {
        return;
    };
    let findings = disk.engine().repair_inspect().unwrap();
    assert_eq!(findings.hostname, "rodecaster-pro-ii");
    assert!(findings.settings_ok);
    assert_eq!(findings.roots.len(), 2);
    for root in &findings.roots {
        assert_eq!(root.reference, "the image's file list");
        assert!(root.damaged.is_empty() && !root.unclean, "{root:?}");
    }
    assert_eq!(findings.repairable(), 0);
}

#[test]
fn a_zero_filled_script_is_restored_from_the_other_root_after_a_settings_backup() {
    let Some(disk) = Disk::new([PRO_II, PRO_II]) else {
        return;
    };
    let script = "usr/bin/rode-init-update-env.sh";
    disk.zero(0, script);
    let mut engine = disk.engine();
    let findings = engine.repair_inspect().unwrap();
    assert_eq!(
        findings.roots[0].damaged,
        [DamagedFile {
            path: script.into(),
            why: None
        }]
    );
    assert!(findings.roots[1].damaged.is_empty());
    assert_eq!(findings.repairable(), 1);
    let result = engine.repair(&findings.targets()).unwrap();
    assert_eq!(result.repaired, [script]);
    assert!(result.not_repaired.is_empty());
    assert_eq!(disk.contents(0, script), SCRIPT);
    disk.clean(0);
    // The backup is the settings partition, byte for byte.
    let backup = std::fs::read(&result.backup).unwrap();
    let whole = std::fs::read(&disk.path).unwrap();
    let start = (PARTS[2].0 * MIB) as usize;
    assert!(backup == whole[start..start + (PARTS[2].1 * MIB) as usize]);
    assert!(engine.repair_inspect().unwrap().roots[0].damaged.is_empty());
}

#[test]
fn without_an_identical_copy_nothing_is_written() {
    let Some(disk) = Disk::new([PRO_II, PRO_II]) else {
        return;
    };
    let script = "usr/bin/rode-init-update-env.sh";
    disk.zero(0, script);
    disk.zero(1, script);
    let mut engine = disk.engine();
    let findings = engine.repair_inspect().unwrap();
    assert_eq!(findings.repairable(), 0);
    assert!(
        findings.roots[0].damaged[0]
            .why
            .as_deref()
            .is_some_and(|w| w.contains("no identical copy"))
    );
    assert!(findings.targets().is_empty());
    // Asked anyway, the helper finds no copy either and writes nothing.
    let result = engine
        .repair(&[target(1, script), target(2, script)])
        .unwrap();
    assert!(result.repaired.is_empty() && result.not_repaired.len() == 2);
    assert!(
        result.backup.is_empty(),
        "no backup when nothing is written"
    );
    assert_eq!(disk.contents(0, script), vec![0; SCRIPT.len()]);
}

#[test]
fn an_uncleanly_stopped_system_is_left_alone() {
    let Some(disk) = Disk::new([PRO_II, PRO_II]) else {
        return;
    };
    let Some(debugfs) = tool("debugfs") else {
        return;
    };
    let script = "usr/bin/rode-init-update-env.sh";
    disk.zero(0, script);
    // Mark root 1's journal as not replayed, in a copy written back.
    let bytes = std::fs::read(&disk.path).unwrap();
    let (start, len) = ((PARTS[0].0 * MIB) as usize, (PARTS[0].1 * MIB) as usize);
    let image = disk.dir.path().join("p1.img");
    std::fs::write(&image, &bytes[start..start + len]).unwrap();
    let status = Command::new(debugfs)
        .args(["-w", "-R", "feature needs_recovery"])
        .arg(&image)
        .output()
        .unwrap();
    assert!(status.status.success());
    write_at(
        &disk.path,
        PARTS[0].0 * MIB,
        &std::fs::read(&image).unwrap(),
    );
    let mut engine = disk.engine();
    let findings = engine.repair_inspect().unwrap();
    assert!(findings.roots[0].unclean);
    assert!(
        findings.roots[0].damaged[0]
            .why
            .as_deref()
            .unwrap()
            .contains("shut down cleanly")
    );
    assert_eq!(findings.repairable(), 0);
    let result = engine.repair(&[target(1, script)]).unwrap();
    assert!(result.not_repaired[0].1.contains("shut down cleanly"));
    assert_eq!(disk.contents(0, script), vec![0; SCRIPT.len()]);
}

#[test]
fn a_disk_that_is_not_a_rodecaster_is_refused_before_anything_is_written() {
    let other = || Tree {
        hostname: "some-laptop",
        manifest: true,
    };
    let Some(disk) = Disk::new([other(), other()]) else {
        return;
    };
    let error = disk
        .engine()
        .repair(&[target(1, "usr/bin/rode-init-update-env.sh")])
        .unwrap_err();
    assert!(error.0.contains("RØDECaster"), "{error}");
    assert!(
        !disk.dir.path().join("work/repair").exists(),
        "no backup either"
    );
    // A disk without a partition table.
    write_at(&disk.path, 510, &[0, 0]);
    assert!(disk.engine().repair_inspect().is_err());
}

#[test]
fn a_root_without_a_file_list_is_compared_with_the_flashed_image_saved_here() {
    let old = || Tree {
        hostname: "rodecaster-pro-ii",
        manifest: false,
    };
    let Some(disk) = Disk::new([old(), old()]) else {
        return;
    };
    let script = "usr/bin/rode-init-update-env.sh";
    disk.zero(0, script);
    disk.zero(1, script);
    // Without the saved image there is nothing to compare with.
    let findings = disk.engine().repair_inspect().unwrap();
    assert!(findings.roots[0].reference.starts_with("nothing"));
    assert!(findings.roots[0].damaged.is_empty());
    // The image that was flashed: same RC2FX checksums, intact script.
    let archive = disk.dir.path().join("work/archive.tar.gz");
    std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        std::fs::File::create(&archive).unwrap(),
        flate2::Compression::fast(),
    ));
    for (name, data) in [
        ("./etc/hostname", &b"rodecaster-pro-ii\n"[..]),
        ("./usr/bin/rode-init-update-env.sh", SCRIPT),
        ("./opt/rc2fx/SHA256SUMS", b"aa  rc2fxd\n"),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, name, data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    let mut engine = disk.engine();
    let findings = engine.repair_inspect().unwrap();
    assert_eq!(
        findings.roots[0].reference,
        "the firmware image saved on this computer"
    );
    assert_eq!(findings.repairable(), 2);
    let result = engine.repair(&findings.targets()).unwrap();
    assert_eq!(result.repaired.len(), 2);
    assert_eq!(disk.contents(0, script), SCRIPT);
    assert_eq!(disk.contents(1, script), SCRIPT);
    disk.clean(0);
    disk.clean(1);
}

#[test]
fn only_the_files_asked_for_are_restored_and_each_is_checked_again() {
    let Some(disk) = Disk::new([PRO_II, PRO_II]) else {
        return;
    };
    let script = "usr/bin/rode-init-update-env.sh";
    disk.zero(0, script);
    disk.zero(0, "usr/bin/big");
    let mut engine = disk.engine();
    let result = engine
        .repair(&[
            target(1, script),
            // Intact: nothing to write.
            target(2, script),
            // Not in the file list: refused, whatever the app asks.
            target(1, "etc/shadow"),
        ])
        .unwrap();
    assert_eq!(disk.contents(0, script), SCRIPT);
    assert_eq!(
        disk.contents(0, "usr/bin/big"),
        vec![0; big().len()],
        "not asked for"
    );
    assert!(result.repaired.contains(&script.to_owned()));
    assert!(
        result
            .not_repaired
            .iter()
            .any(|(p, why)| p == "etc/shadow" && why.contains("not in this system's file list"))
    );
    disk.clean(0);
    // The backup belongs to whoever runs the installer and only to them.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&result.backup)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}
