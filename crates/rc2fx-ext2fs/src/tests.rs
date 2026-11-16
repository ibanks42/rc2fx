//! Real ext4 images made by `mkfs.ext4` with the features of a Pro II's root
//! partitions, checked afterwards by `e2fsck -fn`. Skipped where e2fsprogs'
//! tools are missing; CI's Ubuntu has them.
use super::*;
use std::path::PathBuf;
use std::process::Command;

fn tool(name: &str) -> Option<PathBuf> {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.exists())
}

fn tools() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let found = (tool("mkfs.ext4")?, tool("e2fsck")?, tool("debugfs")?);
    Some(found)
}

const SCRIPT: &[u8] = b"#!/bin/sh\n# mounts the settings partition\nmount -t vfat /dev/mmcblk1p3 /Application/update-staging\n";

struct Image {
    _dir: tempfile::TempDir,
    path: PathBuf,
    e2fsck: PathBuf,
    debugfs: PathBuf,
}

impl Image {
    /// A Pro II root's features on a small image, with a few files.
    fn new(block: u32) -> Option<Self> {
        let Some((mkfs, e2fsck, debugfs)) = tools() else {
            eprintln!("skipped: e2fsprogs' mkfs.ext4, e2fsck or debugfs is not installed");
            return None;
        };
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("usr/bin")).unwrap();
        std::fs::create_dir_all(tree.join("etc")).unwrap();
        std::fs::write(tree.join("usr/bin/rode-init-update-env.sh"), SCRIPT).unwrap();
        std::fs::write(tree.join("etc/hostname"), b"rodecaster-pro-ii\n").unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|n| (n * 7 % 251) as u8).collect();
        std::fs::write(tree.join("usr/bin/big"), &big).unwrap();
        std::os::unix::fs::symlink("big", tree.join("usr/bin/link")).unwrap();
        let path = dir.path().join("root.img");
        let status = Command::new(&mkfs)
            .args(["-F", "-q", "-t", "ext4", "-b", &block.to_string(), "-I", "256"])
            .args([
                "-O",
                "metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file,^metadata_csum_seed",
            ])
            .arg("-d")
            .arg(&tree)
            .arg(&path)
            .arg("8M")
            .status()
            .unwrap();
        assert!(status.success(), "mkfs.ext4");
        Some(Self {
            _dir: dir,
            path,
            e2fsck,
            debugfs,
        })
    }

    fn clean(&self) {
        let out = Command::new(&self.e2fsck)
            .args(["-f", "-n"])
            .arg(&self.path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "e2fsck -fn: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn debugfs(&self, request: &str) {
        let out = Command::new(&self.debugfs)
            .args(["-w", "-R", request])
            .arg(&self.path)
            .output()
            .unwrap();
        assert!(out.status.success(), "debugfs {request}");
    }
}

#[test]
fn a_file_is_found_read_and_restored_in_place() {
    for block in [1024, 4096] {
        let Some(image) = Image::new(block) else {
            return;
        };
        let name = "usr/bin/rode-init-update-env.sh";
        let mut fs = Filesystem::open(&image.path, 0, true).unwrap();
        assert!(!fs.needs_recovery());
        let entry = fs.lookup(name).unwrap().unwrap();
        assert!(entry.regular && entry.size == SCRIPT.len() as u64);
        assert_eq!(fs.read(&entry).unwrap(), SCRIPT);
        // What the flash left: the right size, all zeros.
        fs.overwrite(&entry, &vec![0; SCRIPT.len()]).unwrap();
        fs.close().unwrap();
        image.clean();
        let mut fs = Filesystem::open(&image.path, 0, true).unwrap();
        let entry = fs.lookup(name).unwrap().unwrap();
        assert_eq!(fs.read(&entry).unwrap(), vec![0; SCRIPT.len()]);
        fs.overwrite(&entry, SCRIPT).unwrap();
        fs.close().unwrap();
        image.clean();
        let fs = Filesystem::open(&image.path, 0, false).unwrap();
        let entry = fs.lookup(name).unwrap().unwrap();
        assert_eq!(fs.read(&entry).unwrap(), SCRIPT, "block size {block}");
        // A file spanning many blocks reads back whole.
        let big = fs.lookup("usr/bin/big").unwrap().unwrap();
        assert_eq!(fs.read(&big).unwrap().len(), 300_000);
    }
}

#[test]
fn preallocated_blocks_that_were_never_written_are_filled() {
    // fallocate leaves extents marked unwritten, which read as zeros: one
    // way a file can come out of an interrupted write.
    let Some(image) = Image::new(1024) else {
        return;
    };
    image.debugfs("write /dev/null usr/bin/prealloc");
    image.debugfs("fallocate usr/bin/prealloc 0 7");
    image.debugfs(&format!("sif usr/bin/prealloc size {}", 8 * 1024));
    image.clean();
    let data: Vec<u8> = (0..8 * 1024u32).map(|n| (n % 253) as u8).collect();
    let mut fs = Filesystem::open(&image.path, 0, true).unwrap();
    let entry = fs.lookup("usr/bin/prealloc").unwrap().unwrap();
    assert_eq!(fs.read(&entry).unwrap(), vec![0; data.len()]);
    fs.overwrite(&entry, &data).unwrap();
    fs.close().unwrap();
    image.clean();
    let fs = Filesystem::open(&image.path, 0, false).unwrap();
    let entry = fs.lookup("usr/bin/prealloc").unwrap().unwrap();
    assert_eq!(fs.read(&entry).unwrap(), data);
}

#[test]
fn a_filesystem_inside_a_disk_is_opened_at_its_offset() {
    let Some(image) = Image::new(1024) else {
        return;
    };
    let disk = image.path.with_file_name("disk.img");
    let mut bytes = vec![0u8; 1 << 20];
    bytes.extend(std::fs::read(&image.path).unwrap());
    std::fs::write(&disk, bytes).unwrap();
    let fs = Filesystem::open(&disk, 1 << 20, false).unwrap();
    let entry = fs.lookup("etc/hostname").unwrap().unwrap();
    assert_eq!(fs.read(&entry).unwrap(), b"rodecaster-pro-ii\n");
    assert!(
        Filesystem::open(&disk, 0, false).is_err(),
        "no filesystem at 0"
    );
}

#[test]
fn lookups_and_writes_refuse_what_the_repair_must_not_touch() {
    let Some(image) = Image::new(1024) else {
        return;
    };
    let mut fs = Filesystem::open(&image.path, 0, false).unwrap();
    assert_eq!(fs.lookup("usr/bin/missing").unwrap(), None);
    let dir = fs.lookup("usr/bin").unwrap().unwrap();
    assert!(!dir.regular);
    assert!(fs.read(&dir).is_err());
    let link = fs.lookup("usr/bin/link").unwrap().unwrap();
    assert!(!link.regular, "a final symlink is not followed");
    let entry = fs.lookup("etc/hostname").unwrap().unwrap();
    assert!(
        fs.overwrite(&entry, b"rodecaster-pro-ii\n").is_err(),
        "read-only"
    );
    drop(fs);
    let mut fs = Filesystem::open(&image.path, 0, true).unwrap();
    assert!(fs.overwrite(&entry, b"shorter").is_err(), "size must match");
    fs.close().unwrap();
    image.clean();
}

#[test]
fn an_unreplayed_journal_is_reported() {
    let Some(image) = Image::new(1024) else {
        return;
    };
    image.debugfs("feature needs_recovery");
    let fs = Filesystem::open(&image.path, 0, false).unwrap();
    assert!(fs.needs_recovery());
}
