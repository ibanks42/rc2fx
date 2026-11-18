//go:build windows

package platform

import "testing"

func TestVolumePaths(t *testing.T) {
	root, err := DriveRoot("e:")
	if err != nil || root != `E:\` {
		t.Fatalf("%q %v", root, err)
	}
	if VolumePath(root) != `\\.\E:` {
		t.Fatal(VolumePath(root))
	}
	if _, err := DriveRoot(`E:\x`); err == nil {
		t.Fatal("a path accepted as a drive root")
	}
}
