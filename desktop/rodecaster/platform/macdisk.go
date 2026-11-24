package platform

// macOS update-disk identification from `ioreg` and `diskutil` property
// lists. Parses command output directly, so tests run on every platform.

import (
	"math"
	"strings"

	"howett.net/plist"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

// ParsePlist reads an XML (or binary) property list into nested
// map[string]any / []any values.
func ParsePlist(data []byte) (any, error) {
	var v any
	if _, err := plist.Unmarshal(data, &v); err != nil {
		return nil, err
	}
	return v, nil
}

// ValidDiskName is a BSD disk name such as `disk4` or `disk4s1`.
func ValidDiskName(n string) bool {
	if len(n) <= 4 || !strings.HasPrefix(n, "disk") {
		return false
	}
	for _, c := range []byte(n[4:]) {
		if !(c >= '0' && c <= '9' || c == 's') {
			return false
		}
	}
	return true
}

// plistInteger is a property list integer that fits an i64.
func plistInteger(v any) (int64, bool) {
	switch n := v.(type) {
	case int64:
		return n, true
	case uint64:
		if n > math.MaxInt64 {
			return 0, false
		}
		return int64(n), true
	}
	return 0, false
}

// MixerDisk is a BSD disk below a RØDECaster this package knows: the
// mixer's USB serial and whether it is in update mode.
type MixerDisk struct {
	Serial string
	Update bool
}

// MixerDisks maps `/dev/diskN[sM]` to the mixer above it, in update or
// normal mode, from `ioreg -a -l -r -c IOUSBHostDevice`.
func MixerDisks(tree any) map[string]MixerDisk {
	out := map[string]MixerDisk{}
	var walk func(node any, above MixerDisk)
	walk = func(node any, above MixerDisk) {
		switch n := node.(type) {
		case []any:
			for _, v := range n {
				walk(v, above)
			}
		case map[string]any:
			if _, ok := n["idVendor"]; ok {
				vendor, vendorOK := plistInteger(n["idVendor"])
				product, productOK := plistInteger(n["idProduct"])
				mixer, update := false, false
				if vendorOK && productOK && vendor >= 0 && vendor <= 0xffff && product >= 0 && product <= 0xffff {
					_, mixer = model.FromUSB(uint16(vendor), uint16(product))
					update = IsUpdatePID(uint16(product))
				}
				own, hasOwn := n["USB Serial Number"].(string)
				// Nodes below the device can repeat its IDs without the serial,
				// so they keep the one from above.
				switch {
				case !mixer:
					above = MixerDisk{}
				case hasOwn:
					above = MixerDisk{Serial: own, Update: update}
				}
			}
			if name, ok := n["BSD Name"].(string); above.Serial != "" && ok && ValidDiskName(name) {
				out["/dev/"+name] = above
			}
			if children, ok := n["IORegistryEntryChildren"]; ok {
				walk(children, above)
			}
		}
	}
	walk(tree, MixerDisk{})
	return out
}

// USBDisks maps `/dev/diskN[sM]` to the serial of the mixer in update mode
// above it, from `ioreg -a -l -r -c IOUSBHostDevice`.
func USBDisks(tree any) map[string]string {
	out := map[string]string{}
	for name, d := range MixerDisks(tree) {
		if d.Update {
			out[name] = d.Serial
		}
	}
	return out
}

// DiskInfo is the mount point, FAT detection and capacity from
// `diskutil info -plist`.
type DiskInfo struct {
	MountPoint string
	FAT        bool
	Total      uint64
	Free       uint64
}

// ParseDiskInfo reads `diskutil info -plist`; false when it is not a
// dictionary.
func ParseDiskInfo(info any) (DiskInfo, bool) {
	d, ok := info.(map[string]any)
	if !ok {
		return DiskInfo{}, false
	}
	text := func(k string) string {
		s, _ := d[k].(string)
		return s
	}
	out := DiskInfo{MountPoint: text("MountPoint"), FAT: text("FilesystemType") == "msdos"}
	if n, ok := plistInteger(d["TotalSize"]); ok && n > 0 {
		out.Total = uint64(n)
	}
	if n, ok := plistInteger(d["VolumeFreeSpace"]); ok && n >= 0 {
		out.Free = uint64(n)
	}
	return out, true
}
