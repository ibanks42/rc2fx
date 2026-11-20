package autofix

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/carrier"
)

// minFourKSectors is the smallest partition, in 512-byte sectors, that
// RØDE's updater (mkfs.ext4 without -b, the stock mke2fs.conf) formats with
// 4 KB blocks: 512 MiB. Below it the slot gets 1 KB blocks, on which flashes
// have lost the tails of files.
const minFourKSectors = 512 << 20 >> 9

// NotApplied is the error of a run that stopped before its last flash
// because the slot that flash would format is not safe (lastFlashSafe).
// Summary says what the fix reported and what the person should do.
type NotApplied struct{ Summary string }

func (e *NotApplied) Error() string { return e.Summary }

// lastFlashSafe reports whether the slot the next flash formats (the one
// the mixer is not running when the marker was written) is large enough
// for 4 KB blocks, from the script's own marker. why says what is missing
// when it is not.
func lastFlashSafe(m *carrier.Marker) (bool, string) {
	if m == nil {
		return false, "the fix did not report which system slot the mixer runs or how its storage is laid out"
	}
	target := 0
	switch m.Slot {
	case 1:
		target = 2
	case 2:
		target = 1
	default:
		return false, "the fix could not tell which system slot the mixer runs"
	}
	sectors, ok := tableSize(m.Table, target)
	if !ok {
		return false, fmt.Sprintf("the fix did not report the size of system slot %d", target)
	}
	if sectors < minFourKSectors {
		return false, fmt.Sprintf("system slot %d is still its original size", target)
	}
	return true, ""
}

// tableSize is partition n's size in sectors from the marker's `sfdisk -d`
// lines, such as `/dev/mmcblk1p2 : start= 2000896, size= 1193984, type=83`.
func tableSize(lines []string, n int) (uint64, bool) {
	suffix := "p" + strconv.Itoa(n)
	for _, line := range lines {
		device, fields, ok := strings.Cut(line, ":")
		if !ok || !strings.HasSuffix(strings.TrimSpace(device), suffix) {
			continue
		}
		for _, field := range strings.Split(fields, ",") {
			key, value, ok := strings.Cut(field, "=")
			if !ok || strings.TrimSpace(key) != "size" {
				continue
			}
			size, err := strconv.ParseUint(strings.TrimSpace(value), 10, 64)
			return size, err == nil
		}
	}
	return 0, false
}
