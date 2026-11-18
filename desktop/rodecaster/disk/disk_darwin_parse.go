package disk

// The macOS registry parsing. This file has no build constraint (a "_parse"
// suffix is not a GOOS), so its tests run on every system.

import (
	"fmt"
	"strings"

	"howett.net/plist"
)

// <sys/disk.h> ioctls. _IO is IOC_VOID (0x20000000); _IOR is IOC_OUT
// (0x40000000) with the argument size in bits 16..28.
const (
	macDKIOCSynchronizeCache = 0x20000000 | 'd'<<8 | 22         // _IO('d', 22)
	macDKIOCGetBlockSize     = 0x40000000 | 4<<16 | 'd'<<8 | 24 // _IOR('d', 24, uint32_t)
	macDKIOCGetBlockCount    = 0x40000000 | 8<<16 | 'd'<<8 | 25 // _IOR('d', 25, uint64_t)
)

// macDisk is a whole disk below a mixer in the I/O registry.
type macDisk struct {
	BSDName string // "disk4"
	Size    uint64 // bytes
}

// macParseIoreg reads `ioreg -r -c IOUSBHostDevice -l -a`.
func macParseIoreg(data []byte) ([]macDisk, error) {
	var tree any
	if _, err := plist.Unmarshal(data, &tree); err != nil {
		return nil, err
	}
	return macMixerDisks(tree), nil
}

// macMixerDisks lists the whole disks that sit below a USB device whose
// idVendor and idProduct are the mixer's storage gadget. Nodes between the
// device and its disk (the interface, the mass-storage nub) may repeat the
// device's IDs; any node with an idVendor of its own decides afresh, so a
// disk is only ever credited to its nearest USB device.
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
					// What is below a whole disk is its partitions.
					return
				}
			}
			walk(n["IORegistryEntryChildren"], mixer)
		}
	}
	walk(tree, false)
	return out
}

// macWholeDisk is an IOMedia node for a whole disk: BSD Name "diskN",
// Whole = true and a Size in whole sectors.
func macWholeDisk(n map[string]any) (macDisk, bool) {
	name, _ := n["BSD Name"].(string)
	whole, _ := n["Whole"].(bool)
	size, sizeOK := macPlistUint(n["Size"])
	if !whole || !sizeOK || !macWholeName(name) || size == 0 || size%SectorSize != 0 {
		return macDisk{}, false
	}
	return macDisk{BSDName: name, Size: size}, true
}

// macWholeName is a whole-disk BSD name, "disk" and digits ("disk4", not
// "disk4s1").
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

// macPlistUint is a non-negative property-list integer. The plist package
// gives int64 for integers it read as signed and uint64 otherwise.
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

// macDescribe is Info's Name for a disk: "disk4, 7.3 GB".
func macDescribe(d macDisk) string {
	return fmt.Sprintf("%s, %.1f GB", d.BSDName, float64(d.Size)/1e9)
}
