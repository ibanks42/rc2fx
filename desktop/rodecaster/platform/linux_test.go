//go:build linux

package platform

import (
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"unsafe"
)

func TestMountEscapesAndSCSIHeader(t *testing.T) {
	mounts, err := ParseMounts("36 25 8:17 / /run/media/a\\040b rw,nosuid - vfat /dev/sdb1 rw\n")
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(mounts, []Mount{{Device: "/dev/sdb1", Path: "/run/media/a b", FS: "vfat"}}) {
		t.Fatalf("%+v", mounts)
	}
	if _, err := ParseMounts("garbage\n"); err == nil {
		t.Fatal("garbage parsed")
	}
	// sg_io_hdr_t is 88 bytes on 64-bit Linux.
	if unsafe.Sizeof(uintptr(0)) == 8 && unsafe.Sizeof(SgHeader{}) != 88 {
		t.Fatalf("SgHeader is %d bytes", unsafe.Sizeof(SgHeader{}))
	}
}

func TestMediaAbsenceRequiresExplicitSense(t *testing.T) {
	check := &SgHeader{Status: 2, Info: 1}
	fixed := make([]byte, 18)
	fixed[0], fixed[2], fixed[7], fixed[12] = 0x70, 2, 10, 0x3a
	if got := ScsiStatus(check, fixed); got.Kind != ScsiNoMedium {
		t.Fatalf("%+v", got)
	}
	fixed[2] = 6
	if got := ScsiStatus(check, fixed); got.Kind != ScsiUnitAttention {
		t.Fatalf("%+v", got)
	}
	if got := ScsiStatus(check, nil); got.Kind != ScsiFailed {
		t.Fatalf("%+v", got)
	}
	if got := ScsiStatus(&SgHeader{}, nil); got.Kind != ScsiOK {
		t.Fatalf("%+v", got)
	}
}

// The acknowledgement goes only onto the mixer's disk the volume names: a
// device that is not (or is no longer) a RØDE disk is refused before
// anything is mounted or written, as are names outside the disk's root.
func TestWriteReportRefusesAnotherDisk(t *testing.T) {
	h := host{}
	for _, v := range []Volume{
		{Device: "/dev/null", Serial: "RCP2X1234567"},
		{Device: "/dev/rc2fx-no-such-disk", Serial: "RCP2X1234567"},
		{Device: filepath.Join(t.TempDir(), "disk"), Serial: "RCP2X1234567"},
	} {
		err := h.WriteReport(v, "RCFIX.ACK", []byte("ack=1\n"))
		if err == nil || !strings.Contains(err.Error(), "identity changed") {
			t.Errorf("%s: %v", v.Device, err)
		}
	}
	for _, name := range []string{"", ".", "..", "a/RCFIX.ACK", "../RCFIX.ACK"} {
		if err := h.WriteReport(Volume{Device: "/dev/null"}, name, nil); err == nil {
			t.Errorf("name %q was accepted", name)
		}
	}
}
