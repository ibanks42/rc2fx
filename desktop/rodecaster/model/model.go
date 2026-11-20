// Package model defines hardware metadata for each RØDECaster model (names,
// USB IDs, board identity, and update manifest URLs).
package model

import (
	"fmt"
	"slices"
)

// Model identifies a RØDECaster hardware model (zero value is ProIi).
type Model uint8

const (
	ProIi Model = iota
	Duo
)

var All = [2]Model{ProIi, Duo}

// VendorID is RØDE's USB vendor ID.
const VendorID uint16 = 0x19f7

func Parse(id string) (Model, error) {
	for _, m := range All {
		if m.ID() == id {
			return m, nil
		}
	}
	return 0, fmt.Errorf("unknown mixer model %s", id)
}

func (m Model) ID() string {
	if m == Duo {
		return "rodecaster-duo"
	}
	return "rodecaster-pro-ii"
}

func (m Model) String() string { return m.ID() }

func (m Model) Label() string {
	if m == Duo {
		return "RØDECaster Duo"
	}
	return "RØDECaster Pro II"
}

func (m Model) Hostname() string {
	if m == Duo {
		return "rodecaster-pro-ii-mini"
	}
	return "rodecaster-pro-ii"
}

func (m Model) BoardDTB() string {
	if m == Duo {
		return "/boot/meson-axg-rodecaster-pro-ii-mini.dtb"
	}
	return "/boot/meson-axg-rodecaster-pro-ii.dtb"
}

func (m Model) FirmwareDirectory() string {
	if m == Duo {
		return "rcp2-duo"
	}
	return "rcp2"
}

func (m Model) NormalPIDs() [5]uint16 {
	if m == Duo {
		return [5]uint16{0x0050, 0x0079, 0x008e, 0x0093, 0x0095}
	}
	return [5]uint16{0x0037, 0x0078, 0x008d, 0x0092, 0x0094}
}

func (m Model) UpdatePID() uint16 {
	if m == Duo {
		return 0x004f
	}
	return 0x0030
}

// ManifestURL returns the RØDE update manifest URL for this model.
func (m Model) ManifestURL() string {
	if m == Duo {
		return "https://update.rode.com/rcp2-mini-manifest.json"
	}
	return "https://update.rode.com/manifest.json"
}

// FromUSB matches a USB vendor/product ID pair in either normal or update mode.
func FromUSB(vendor, product uint16) (Model, bool) {
	if vendor != VendorID {
		return 0, false
	}
	for _, m := range All {
		pids := m.NormalPIDs()
		if slices.Contains(pids[:], product) || m.UpdatePID() == product {
			return m, true
		}
	}
	return 0, false
}
