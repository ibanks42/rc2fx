package model

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"testing"
)

// config/udev/70-rc2fx.rules gives the logged-in user access to the mixer's
// HID interfaces in normal mode; it must name exactly the normal-mode product
// IDs of every model, under RØDE's vendor ID.
func TestUdevRulesCoverEveryNormalPID(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "..", "config", "udev", "70-rc2fx.rules"))
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, m := range regexp.MustCompile(`ATTRS\{idVendor\}=="([0-9a-f]{4})", ATTRS\{idProduct\}=="([0-9a-f]{4})"`).FindAllStringSubmatch(string(data), -1) {
		got = append(got, m[1]+":"+m[2])
	}
	var want []string
	for _, m := range All {
		for _, pid := range m.NormalPIDs() {
			want = append(want, fmt.Sprintf("%04x:%04x", VendorID, pid))
		}
	}
	slices.Sort(got)
	slices.Sort(want)
	if !slices.Equal(got, want) {
		t.Fatalf("70-rc2fx.rules\n%v\nnormal-mode PIDs\n%v", got, want)
	}
}
