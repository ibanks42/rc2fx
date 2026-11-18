// Package model holds what a flashing tool needs to know about each
// RØDECaster model: its names, USB IDs, board identity and RØDE's update
// manifest.
package model

import (
	"fmt"
	"slices"
)

// Model is a mixer model. The zero value is ProIi.
type Model uint8

const (
	ProIi Model = iota
	Duo
)

// All is every model, in a fixed order.
var All = [2]Model{ProIi, Duo}

// VendorID is RØDE's USB vendor ID.
const VendorID uint16 = 0x19f7

// Parse finds the model with the given ID.
func Parse(id string) (Model, error) {
	for _, m := range All {
		if m.ID() == id {
			return m, nil
		}
	}
	return 0, fmt.Errorf("unknown mixer model %s", id)
}

// ID is the model's catalog and serialized name.
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

// ManifestURL is RØDE's update manifest for the model, the list the
// official updater reads.
func (m Model) ManifestURL() string {
	if m == Duo {
		return "https://update.rode.com/rcp2-mini-manifest.json"
	}
	return "https://update.rode.com/manifest.json"
}

// FromUSB identifies a model by its USB IDs, in normal or update mode.
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
