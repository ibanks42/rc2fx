//! What the installer records about a patched image so the mixer can check
//! its own copy, and whether the image fits RØDE's smaller root partition.
//!
//! RØDE's updater unpacks the image into whichever of its two root
//! partitions is not running. A flash once left one of RØDE's boot scripts
//! zero-filled on the new root: the right size, none of the contents. The
//! mixer then booted without its settings partition and lost its setup at
//! every start. The manifest lets `rc2fx.sh` find such a file on the first
//! boots and restore it from the other root when that copy is identical.
use crate::{Result, bail};

/// Where the manifest goes in the image. `rc2fx.sh image-check` reads it.
pub(crate) const MANIFEST_MEMBER: &str = "opt/rc2fx/image.sha256";

/// RØDE files the mixer rewrites or removes on its own, so their contents on
/// a running mixer say nothing about the flash. RØDE's updater
/// (`rode-update.sh`) points `extlinux.conf` at the second root when it
/// installs there; "restoring" it would make that root boot the other one.
const VOLATILE: [&str; 6] = [
    crate::KEY_MEMBER,
    "boot/extlinux/extlinux.conf",
    "etc/asound.conf",
    "etc/timestamp",
    "usr/bin/first-boot",
    "var/lib/alsa/asound.state",
];

/// The two root filesystems RØDE's updater creates with `mkfs.ext4` on a
/// Pro II, as (block size, blocks) from `statfs` on 1.7.7. The first is the
/// 507 MB partition, the second the 583 MB one. The Duo uses the same scripts.
const ROOTS: [(u64, u64); 2] = [(1024, 476_546), (4096, 142_585)];

/// Space the root must keep free after unpacking, in bytes. RØDE writes
/// small files there at run time, and its update-mode drive needs the
/// settings partition, not the root; this keeps a full root from ever being
/// the first thing that breaks.
pub(crate) const ROOT_HEADROOM: u64 = 48 << 20;

/// Whether a member belongs in the manifest: RØDE's own regular files, minus
/// the ones it changes itself. RC2FX's files are checked by its SHA256SUMS.
pub(crate) fn listed(name: &str) -> bool {
    !VOLATILE.contains(&name)
        && !name.starts_with("opt/rc2fx/")
        && !name.is_empty()
        && !name.bytes().any(|b| b == b'\n' || b == b'\\' || b == 0)
}

/// One `sha256sum -c` line, relative to `/`.
pub(crate) fn line(sha: &str, name: &str) -> String {
    format!("{sha}  {name}\n")
}

/// The space an image takes once unpacked, for each root block size.
#[derive(Default)]
pub(crate) struct RootUsage {
    by_root: [u64; ROOTS.len()],
}

impl RootUsage {
    /// A regular file's data, rounded up to whole blocks.
    pub fn file(&mut self, size: u64) {
        for (used, (block, _)) in self.by_root.iter_mut().zip(ROOTS) {
            *used += size.div_ceil(block) * block;
        }
    }

    /// A directory takes one block; ext4 keeps symlinks under 60 bytes in
    /// the inode and longer ones in a block.
    pub fn block(&mut self) {
        for (used, (block, _)) in self.by_root.iter_mut().zip(ROOTS) {
            *used += block;
        }
    }

    pub fn symlink(&mut self, target_len: usize) {
        if target_len >= 60 {
            self.block();
        }
    }

    /// Refuse an image that would leave either root with less than
    /// [`ROOT_HEADROOM`] free. On 1.7.7 this estimate came within 0.3 MiB of
    /// the space used on both roots of a Pro II.
    pub fn check(&self) -> Result<()> {
        for (used, (block, blocks)) in self.by_root.iter().zip(ROOTS) {
            let capacity = block * blocks;
            let free = capacity.saturating_sub(*used);
            if free < ROOT_HEADROOM {
                bail!(
                    "the firmware with RC2FX would leave {} MB free on the mixer's {} MB system partition; it needs at least {} MB, so nothing was flashed",
                    free >> 20,
                    capacity >> 20,
                    ROOT_HEADROOM >> 20
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_space_estimate_rounds_to_each_roots_blocks() {
        let mut usage = RootUsage::default();
        usage.file(1);
        usage.block();
        usage.symlink(10);
        usage.symlink(60);
        assert_eq!(usage.by_root, [3 * 1024, 3 * 4096]);
    }

    #[test]
    fn an_image_that_leaves_too_little_room_is_refused() {
        let mut usage = RootUsage::default();
        // The 0.2.0 image on 1.7.7: about 375 MiB on the smaller root.
        usage.file(375 << 20);
        usage.check().unwrap();
        // Growing past the headroom on the smaller root is refused.
        usage.file(50 << 20);
        let error = usage.check().unwrap_err().to_string();
        assert!(error.contains("465 MB system partition"), "{error}");
        assert!(error.contains("nothing was flashed"), "{error}");
    }

    #[test]
    fn the_manifest_leaves_out_what_the_mixer_rewrites_and_rc2fx_files() {
        assert!(listed("usr/bin/rode-init-update-env.sh"));
        assert!(listed("usr/bin/audio-start.sh"));
        for name in VOLATILE {
            assert!(!listed(name), "{name}");
        }
        assert!(!listed("opt/rc2fx/rc2fxd"));
        assert!(!listed("odd\nname"));
        assert_eq!(line("ab", "etc/hostname"), "ab  etc/hostname\n");
    }
}
