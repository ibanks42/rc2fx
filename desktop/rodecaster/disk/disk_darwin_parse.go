package disk

// macOS ioreg plist parsing, kept platform-agnostic so tests run everywhere.

import (
	"fmt"
	"strings"

	"howett.net/plist"
)

// <sys/disk.h> ioctls (_IO = IOC_VOID 0x20000000, _IOR = IOC_OUT 0x40000000).
const (
	macDKIOCSynchronizeCache = 0x20000000 | 'd'<<8 | 22         // _IO('d', 22)
	macDKIOCGetBlockSize     = 0x40000000 | 4<<16 | 'd'<<8 | 24 // _IOR('d', 24, uint32_t)
	macDKIOCGetBlockCount    = 0x40000000 | 8<<16 | 'd'<<8 | 25 // _IOR('d', 25, uint64_t)
)

type macDisk struct {
	BSDName string // e.g. "disk4"
	Size    uint64 // bytes
}

// macParseIoreg parses the XML plist output of `ioreg -r -c IOUSBHostDevice -l -a`.
func macParseIoreg(data []byte) ([]macDisk, error) {
	var tree any
	if _, err := plist.Unmarshal(data, &tree); err != nil {
		return nil, err
	}
	return macMixerDisks(tree), nil
}

// macMixerDisks finds whole IOMedia disks underneath a mixer USB device.
// Any intermediate node that declares its own idVendor overrides the inherited
// match so a disk is always attributed to its nearest USB ancestor.
func macMixerDisks(tree any) []macDisk {
	var out []macDisk
	seen := map[string]bool{}
	var walk func(node any, mixer bool)
	walk = func(node any, mixer bool) {
		switch n := node.(type) {
		case []any:
			for _, child := range n {
				walk(child, mixer)
			}
		case map[string]any:
			if _, ok := n["idVendor"]; ok {
				vendor, vendorOK := macPlistUint(n["idVendor"])
				product, productOK := macPlistUint(n["idProduct"])
				mixer = vendorOK && productOK && vendor == USBVendor && product == USBProduct
			}
			if mixer {
				if d, ok := macWholeDisk(n); ok {
					if !seen[d.BSDName] {
						seen[d.BSDName] = true
						out = append(out, d)
					}
					// Skip children (partition slices).
					return
				}
			}
			walk(n["IORegistryEntryChildren"], mixer)
		}
	}
	walk(tree, false)
	return out
}

func macWholeDisk(n map[string]any) (macDisk, bool) {
	name, _ := n["BSD Name"].(string)
	whole, _ := n["Whole"].(bool)
	size, sizeOK := macPlistUint(n["Size"])
	if !whole || !sizeOK || !macWholeName(name) || size == 0 || size%SectorSize != 0 {
		return macDisk{}, false
	}
	return macDisk{BSDName: name, Size: size}, true
}

func macWholeName(name string) bool {
	digits, ok := strings.CutPrefix(name, "disk")
	if !ok || len(digits) == 0 || len(digits) > 6 {
		return false
	}
	for _, c := range []byte(digits) {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}

func macPlistUint(v any) (uint64, bool) {
	switch n := v.(type) {
	case int64:
		if n < 0 {
			return 0, false
		}
		return uint64(n), true
	case uint64:
		return n, true
	}
	return 0, false
}

func macDescribe(d macDisk) string {
	return fmt.Sprintf("%s, %.1f GB", d.BSDName, float64(d.Size)/1e9)
}
