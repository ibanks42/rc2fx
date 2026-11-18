package platform

import (
	"fmt"
	"testing"
)

func plistTree(t *testing.T, text string) any {
	t.Helper()
	v, err := ParsePlist([]byte(text))
	if err != nil {
		t.Fatal(err)
	}
	return v
}

// Shaped like a real Pro II: the interface repeats the device's IDs and
// serial, and the mass-storage nub below it repeats the IDs alone.
func TestFindsDisksBelowAMassStorageNub(t *testing.T) {
	tree := func(pid int) any {
		return plistTree(t, fmt.Sprintf(`<?xml version="1.0"?><plist><array><dict>
		<key>idVendor</key><integer>1452</integer><key>idProduct</key><integer>4</integer>
		<key>USB Serial Number</key><string>HUB</string>
		<key>IORegistryEntryChildren</key><array><dict>
		  <key>idVendor</key><integer>6647</integer><key>idProduct</key><integer>%[1]d</integer>
		  <key>USB Serial Number</key><string>GV0044100</string>
		  <key>IORegistryEntryChildren</key><array><dict>
		    <key>idVendor</key><integer>6647</integer><key>idProduct</key><integer>%[1]d</integer>
		    <key>USB Serial Number</key><string>GV0044100</string>
		    <key>IORegistryEntryChildren</key><array><dict>
		      <key>idVendor</key><integer>6647</integer><key>idProduct</key><integer>%[1]d</integer>
		      <key>IORegistryEntryChildren</key><array><dict>
		        <key>BSD Name</key><string>disk4</string>
		        <key>IORegistryEntryChildren</key><array>
		          <dict><key>BSD Name</key><string>disk4s1</string></dict>
		        </array>
		      </dict></array>
		    </dict></array>
		  </dict></array>
		</dict></array>
		</dict></array></plist>`, pid))
	}
	disks := USBDisks(tree(48))
	if len(disks) != 2 || disks["/dev/disk4"] != "GV0044100" || disks["/dev/disk4s1"] != "GV0044100" {
		t.Fatalf("%v", disks)
	}
	if len(USBDisks(tree(148))) != 0 {
		t.Fatal("not in update mode")
	}
}

func TestIdentifiesUpdateDisksAndDiskInfo(t *testing.T) {
	text := func(pid int) string {
		return fmt.Sprintf(`<?xml version="1.0"?><plist><array><dict>
		<key>idVendor</key><integer>6647</integer><key>idProduct</key><integer>%d</integer>
		<key>USB Serial Number</key><string>GV0044100</string>
		<key>IORegistryEntryChildren</key><array>
		  <dict><key>BSD Name</key><string>disk4</string></dict>
		  <dict><key>BSD Name</key><string>disk4s1</string></dict>
		  <dict><key>BSD Name</key><string>../evil</string></dict>
		  <dict><key>idVendor</key><integer>1</integer><key>IORegistryEntryChildren</key><array>
		    <dict><key>BSD Name</key><string>disk9</string></dict></array></dict>
		</array></dict></array></plist>`, pid)
	}
	disks := USBDisks(plistTree(t, text(48)))
	// Duo update, Duo normal, USB 2.
	for pid, want := range map[int]int{79: 2, 80: 0, 38: 0} {
		if got := len(USBDisks(plistTree(t, text(pid)))); got != want {
			t.Fatalf("%d: %d", pid, got)
		}
	}
	if len(disks) != 2 || disks["/dev/disk4s1"] != "GV0044100" {
		t.Fatalf("%v", disks)
	}
	info, ok := ParseDiskInfo(plistTree(t, `<plist><dict><key>MountPoint</key><string>/Volumes/Test &amp; Space</string>
		<key>FilesystemType</key><string>msdos</string>
		<key>TotalSize</key><integer>283189248</integer></dict></plist>`))
	if !ok || info != (DiskInfo{MountPoint: "/Volumes/Test & Space", FAT: true, Total: 283_189_248}) {
		t.Fatalf("%+v", info)
	}
	if !ValidDiskName("disk12s3") || ValidDiskName("disk") || ValidDiskName("sda") {
		t.Fatal("disk names")
	}
}
