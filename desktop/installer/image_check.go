package installer

// Generates the post-flash integrity manifest and checks that patched images
// fit on RØDE's root partitions.
//
// RØDE's updater unpacks the image into whichever of its two root partitions
// is inactive. In the past a flash left one of RØDE's boot scripts zero-filled
// (right size, all zeros), causing the mixer to boot without its settings
// partition. The manifest lets rc2fx.sh detect corrupted files on boot and
// restore them from the other root when that copy matches.

import (
	"slices"
	"strings"
)

// Written into the patched image and verified by `rc2fx.sh image-check`.
const manifestMember = "opt/rc2fx/image.sha256"

// RØDE files modified or removed at runtime, so we exclude them from the
// manifest. Notably, rode-update.sh rewrites extlinux.conf to point at the
// active root; restoring it from the other root would flip the boot target.
var volatile = [6]string{
	keyMember,
	"boot/extlinux/extlinux.conf",
	"etc/asound.conf",
	"etc/timestamp",
	"usr/bin/first-boot",
	"var/lib/alsa/asound.state",
}

// The two ext4 root partitions created by RØDE's updater as (blockSize, blocks),
// measured via statfs on Pro II 1.7.7 (507 MB and 583 MB; Duo uses the same).
var rootPartitions = [2][2]uint64{{1024, 476_546}, {4096, 142_585}}

// Minimum free space required on each root partition after unpacking.
const rootHeadroom uint64 = 48 << 20

// inManifest selects RØDE's non-volatile regular files (RC2FX's own files are
// checked separately via SHA256SUMS).
func inManifest(name string) bool {
	return !slices.Contains(volatile[:], name) &&
		!strings.HasPrefix(name, "opt/rc2fx/") &&
		name != "" &&
		!strings.ContainsAny(name, "\n\\\x00")
}

func line(sha, name string) string {
	return sha + "  " + name + "\n"
}

// rootUsage tracks estimated unpacked disk usage for each root block size.
type rootUsage struct {
	byRoot [len(rootPartitions)]uint64
}

func (u *rootUsage) file(size uint64) {
	for i, root := range rootPartitions {
		blockSize := root[0]
		u.byRoot[i] += (size + blockSize - 1) / blockSize * blockSize
	}
}

// Directories take one block; ext4 stores symlinks under 60 bytes inline in the
// inode and longer ones in a data block.
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

// check fails if unpacking would leave less than rootHeadroom free on either
// root partition (within ~0.3 MiB of actual usage on Pro II 1.7.7).
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
