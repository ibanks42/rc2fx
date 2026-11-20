package installflow

import (
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Sighting captures what the app can observe about a mixer before connecting
// to RC2FX. Flashed only matters while no mixer is visible on USB (see
// MayBeUpdating); a mixer back in normal mode is no longer updating.
type Sighting struct {
	USB []installer.USBMixer
	// RC2FX is true when the normal-mode mixer exposes RC2FX's control reports,
	// meaning RC2FX is installed and starting up.
	RC2FX bool
	// Storage is true when a mixer's storage is exposed as a disk (only
	// detectable on some platforms).
	Storage bool
	// Flashed is true if a flash was sent within the recent update window.
	Flashed bool
}

// First is the mixer state shown on the first screen before a session connects.
type First int

const (
	FirstNothing   First = iota
	FirstFound           // single Pro II in normal mode without RC2FX
	FirstFinishing       // no mixer on USB shortly after a flash (restarting to finish update)
	FirstStarting        // RC2FX is installed on the mixer but not connected yet
	FirstUpdateMode
	FirstStorage
	FirstTwo
	FirstDuo // single Duo in normal mode without RC2FX
)

// FirstOf classifies a Sighting for the first screen. Storage mode takes
// priority because a mixer in storage mode exposes no other USB device.
func FirstOf(s Sighting) First {
	switch {
	case s.Storage:
		return FirstStorage
	case len(s.USB) > 1:
		return FirstTwo
	case len(s.USB) == 0 && MayBeUpdating(s.Flashed, nil, false):
		return FirstFinishing
	case len(s.USB) == 0:
		return FirstNothing
	}
	mixer := s.USB[0]
	switch {
	case mixer.UpdateMode:
		return FirstUpdateMode
	case s.RC2FX:
		return FirstStarting
	case mixer.Model == firmware.ModelDuo:
		return FirstDuo
	}
	return FirstFound
}
