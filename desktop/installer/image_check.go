package installer

// What the installer records about a patched image so the mixer can check
// its own copy, and whether the image fits RØDE's smaller root partition.
//
// RØDE's updater unpacks the image into whichever of its two root
// partitions is not running. A flash once left one of RØDE's boot scripts
// zero-filled on the new root: the right size, none of the contents. The
// mixer then booted without its settings partition and lost its setup at
// every start. The manifest lets `rc2fx.sh` find such a file on the first
// boots and restore it from the other root when that copy is identical.

import (
	"slices"
	"strings"
)

// manifestMember is where the manifest goes in the image. `rc2fx.sh
// image-check` reads it.
const manifestMember = "opt/rc2fx/image.sha256"

// volatile is RØDE's files the mixer rewrites or removes on its own, so
// their contents on a running mixer say nothing about the flash. RØDE's
// updater (`rode-update.sh`) points `extlinux.conf` at the second root when
// it installs there; "restoring" it would make that root boot the other one.
var volatile = [6]string{
	keyMember,
	"boot/extlinux/extlinux.conf",
	"etc/asound.conf",
	"etc/timestamp",
	"usr/bin/first-boot",
	"var/lib/alsa/asound.state",
}

// rootPartitions is the two root filesystems RØDE's updater
// creates with `mkfs.ext4` on a Pro II, as (block size, blocks) from
// `statfs` on 1.7.7. The first is the 507 MB partition, the second the
// 583 MB one. The Duo uses the same scripts.
var rootPartitions = [2][2]uint64{{1024, 476_546}, {4096, 142_585}}

// rootHeadroom is the space the root must keep free after unpacking, in
// bytes. RØDE writes small files there at run time, and its update-mode
// drive needs the settings partition, not the root; this keeps a full root
// from ever being the first thing that breaks.
const rootHeadroom uint64 = 48 << 20

// inManifest is whether a member belongs in the
// manifest: RØDE's own regular files, minus the ones it changes itself.
// RC2FX's files are checked by its SHA256SUMS.
func inManifest(name string) bool {
	return !slices.Contains(volatile[:], name) &&
		!strings.HasPrefix(name, "opt/rc2fx/") &&
		name != "" &&
		!strings.ContainsAny(name, "\n\\\x00")
}

// line is one `sha256sum -c` line, relative to `/`.
func line(sha, name string) string {
	return sha + "  " + name + "\n"
}

// rootUsage is the space an image takes once unpacked, for each root block
// size.
type rootUsage struct {
	byRoot [len(rootPartitions)]uint64
}

// file is a regular file's data, rounded up to whole blocks.
func (u *rootUsage) file(size uint64) {
	for i, root := range rootPartitions {
		blockSize := root[0]
		u.byRoot[i] += (size + blockSize - 1) / blockSize * blockSize
	}
}

// block is a directory, which takes one block; ext4 keeps symlinks under 60
// bytes in the inode and longer ones in a block.
func (u *rootUsage) block() {
	for i, root := range rootPartitions {
		u.byRoot[i] += root[0]
	}
}

func (u *rootUsage) symlink(targetLen int) {
	if targetLen >= 60 {
		u.block()
	}
}

// check refuses an image that would leave either root with less than
// rootHeadroom free. On 1.7.7 this estimate came within 0.3 MiB of the space
// used on both roots of a Pro II.
func (u *rootUsage) check() error {
	for i, root := range rootPartitions {
		capacity := root[0] * root[1]
		var free uint64
		if capacity > u.byRoot[i] {
			free = capacity - u.byRoot[i]
		}
		if free < rootHeadroom {
			return errorf("the firmware with RC2FX would leave %d MB free on the mixer's %d MB system partition; it needs at least %d MB, so nothing was flashed",
				free>>20, capacity>>20, rootHeadroom>>20)
		}
	}
	return nil
}
