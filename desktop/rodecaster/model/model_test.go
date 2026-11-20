package model

import "testing"

func TestModelValues(t *testing.T) {
	cases := []struct {
		m                                   Model
		id, label, host, dtb, dir, manifest string
		update                              uint16
		firstNormal                         uint16
	}{
		{ProIi, "rodecaster-pro-ii", "RØDECaster Pro II", "rodecaster-pro-ii", "/boot/meson-axg-rodecaster-pro-ii.dtb", "rcp2", "https://update.rode.com/manifest.json", 0x0030, 0x0037},
		{Duo, "rodecaster-duo", "RØDECaster Duo", "rodecaster-pro-ii-mini", "/boot/meson-axg-rodecaster-pro-ii-mini.dtb", "rcp2-duo", "https://update.rode.com/rcp2-mini-manifest.json", 0x004f, 0x0050},
	}
	for _, c := range cases {
		if c.m.ID() != c.id || c.m.String() != c.id || c.m.Label() != c.label || c.m.Hostname() != c.host ||
			c.m.BoardDTB() != c.dtb || c.m.FirmwareDirectory() != c.dir || c.m.ManifestURL() != c.manifest ||
			c.m.UpdatePID() != c.update || c.m.NormalPIDs()[0] != c.firstNormal {
			t.Errorf("model %d values differ", c.m)
		}
		parsed, err := Parse(c.id)
		if err != nil || parsed != c.m {
			t.Errorf("Parse(%q) = %v, %v", c.id, parsed, err)
		}
	}
	if _, err := Parse("rodecaster-pro"); err == nil {
		t.Error("unknown ID parsed")
	}
}

func TestFromUSB(t *testing.T) {
	cases := []struct {
		vendor, product uint16
		want            Model
		ok              bool
	}{
		{VendorID, 0x0037, ProIi, true},
		{VendorID, 0x0094, ProIi, true},
		{VendorID, 0x0030, ProIi, true},
		{VendorID, 0x0050, Duo, true},
		{VendorID, 0x0095, Duo, true},
		{VendorID, 0x004f, Duo, true},
		{VendorID, 0x0001, 0, false},
		{0x1234, 0x0037, 0, false},
	}
	for _, c := range cases {
		m, ok := FromUSB(c.vendor, c.product)
		if ok != c.ok || (ok && m != c.want) {
			t.Errorf("FromUSB(%#x, %#x) = %v, %v", c.vendor, c.product, m, ok)
		}
	}
}
