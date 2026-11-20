package platform

import "testing"

func TestVendorUsageAcceptsOnlyTheVendorPage(t *testing.T) {
	if !VendorUsage([]byte{0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01}) {
		t.Fatal("vendor page refused")
	}
	if VendorUsage([]byte{0x05, 0x01, 0x09, 0x06, 0xa1, 0x01}) {
		t.Fatal("keyboard accepted")
	}
}
