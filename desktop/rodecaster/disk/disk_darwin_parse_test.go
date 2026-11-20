package disk

import (
	"reflect"
	"testing"
)

// Fixture modeled on `ioreg -r -c IOUSBHostDevice -l -a`: one mixer with a
// whole disk and partition, an unrelated USB drive, and a second mixer whose
// disk hasn't enumerated yet.
const macFixture = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
  <dict>
    <key>IOObjectClass</key><string>IOUSBHostDevice</string>
    <key>idVendor</key><integer>7054</integer>
    <key>idProduct</key><integer>64218</integer>
    <key>IORegistryEntryChildren</key>
    <array>
      <dict>
        <key>IOObjectClass</key><string>IOUSBHostInterface</string>
        <key>idVendor</key><integer>7054</integer>
        <key>idProduct</key><integer>64218</integer>
        <key>IORegistryEntryChildren</key>
        <array>
          <dict>
            <key>IOObjectClass</key><string>IOUSBMassStorageInterfaceNub</string>
            <key>IORegistryEntryChildren</key>
            <array>
              <dict>
                <key>IOObjectClass</key><string>IOMedia</string>
                <key>BSD Name</key><string>disk4</string>
                <key>Whole</key><true/>
                <key>Size</key><integer>7818182656</integer>
                <key>IORegistryEntryChildren</key>
                <array>
                  <dict>
                    <key>IOObjectClass</key><string>IOMedia</string>
                    <key>BSD Name</key><string>disk4s1</string>
                    <key>Whole</key><false/>
                    <key>Size</key><integer>532202496</integer>
                  </dict>
                </array>
              </dict>
            </array>
          </dict>
        </array>
      </dict>
    </array>
  </dict>
  <dict>
    <key>IOObjectClass</key><string>IOUSBHostDevice</string>
    <key>idVendor</key><integer>2385</integer>
    <key>idProduct</key><integer>5734</integer>
    <key>IORegistryEntryChildren</key>
    <array>
      <dict>
        <key>IOObjectClass</key><string>IOMedia</string>
        <key>BSD Name</key><string>disk5</string>
        <key>Whole</key><true/>
        <key>Size</key><integer>15938355200</integer>
      </dict>
    </array>
  </dict>
  <dict>
    <key>IOObjectClass</key><string>IOUSBHostDevice</string>
    <key>idVendor</key><integer>7054</integer>
    <key>idProduct</key><integer>64218</integer>
    <key>IORegistryEntryChildren</key>
    <array>
      <dict>
        <key>IOObjectClass</key><string>IOUSBHostInterface</string>
        <key>idVendor</key><integer>7054</integer>
        <key>idProduct</key><integer>64218</integer>
      </dict>
    </array>
  </dict>
</array>
</plist>
`

func TestMacFindsOnlyTheMixersWholeDisk(t *testing.T) {
	disks, err := macParseIoreg([]byte(macFixture))
	if err != nil {
		t.Fatal(err)
	}
	want := []macDisk{{BSDName: "disk4", Size: 7818182656}}
	if !reflect.DeepEqual(disks, want) {
		t.Fatalf("got %+v, want %+v", disks, want)
	}
	if got := macDescribe(disks[0]); got != "disk4, 7.8 GB" {
		t.Fatalf("name %q", got)
	}
}

func TestMacNearestUSBDeviceDecides(t *testing.T) {
	tree := []any{map[string]any{
		"idVendor": uint64(USBVendor), "idProduct": uint64(USBProduct),
		"IORegistryEntryChildren": []any{map[string]any{
			"idVendor": int64(1), "idProduct": int64(2),
			"IORegistryEntryChildren": []any{map[string]any{
				"BSD Name": "disk7", "Whole": true, "Size": uint64(1 << 30),
			}},
		}},
	}}
	if got := macMixerDisks(tree); len(got) != 0 {
		t.Fatalf("%+v", got)
	}
}

func TestMacRejectsOddDisks(t *testing.T) {
	for _, n := range []map[string]any{
		{"BSD Name": "disk4s1", "Whole": true, "Size": uint64(1 << 30)},
		{"BSD Name": "../disk4", "Whole": true, "Size": uint64(1 << 30)},
		{"BSD Name": "disk4", "Size": uint64(1 << 30)},
		{"BSD Name": "disk4", "Whole": true, "Size": uint64(1000)},
		{"BSD Name": "disk4", "Whole": true, "Size": int64(-512)},
		{"BSD Name": "disk4", "Whole": true},
	} {
		if d, ok := macWholeDisk(n); ok {
			t.Fatalf("%v accepted as %+v", n, d)
		}
	}
	if !macWholeName("disk12") || macWholeName("disk") || macWholeName("sda") {
		t.Fatal("disk names")
	}
}

func TestMacIoctlNumbers(t *testing.T) {
	// The values <sys/disk.h> expands to.
	if macDKIOCSynchronizeCache != 0x20006416 || macDKIOCGetBlockSize != 0x40046418 || macDKIOCGetBlockCount != 0x40086419 {
		t.Fatalf("%#x %#x %#x", macDKIOCSynchronizeCache, macDKIOCGetBlockSize, macDKIOCGetBlockCount)
	}
}
