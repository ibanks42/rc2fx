package installflow

// The first screen's decision: which state the mixer is in, from what the
// app can see without a connection to RC2FX. Package app looks (connection.go)
// and draws it.

import (
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Sighting is what the app sees of the mixer before it connects: the
// mixers on USB, whether the one in normal mode declares RC2FX's control
// reports (RC2FX's USB layout is live, so RC2FX is installed and starting),
// whether a mixer's storage shows as a disk (storage mode), and whether a
// flash was sent in the last few minutes. That last one counts only while
// no mixer shows itself (MayBeUpdating): a mixer in normal mode is not
// updating, whatever was sent.
type Sighting struct {
	USB []installer.USBMixer
	// RC2FX: the one mixer in normal mode declares RC2FX's control reports.
	RC2FX bool
	// Storage: a mixer's storage is visible as a disk. Only some systems
	// can see it.
	Storage bool
	// Flashed: the app sent a flash in the last few minutes.
	Flashed bool
}

// First is the state the first screen names.
type First int

const (
	// FirstNothing: no mixer on USB.
	FirstNothing First = iota
	// FirstFound: one Pro II in normal mode, without RC2FX.
	FirstFound
	// FirstFinishing: no mixer on USB just after a flash: it is restarting
	// while it finishes the update.
	FirstFinishing
	// FirstStarting: RC2FX is on the mixer but the app isn't connected to it
	// yet.
	FirstStarting
	// FirstUpdateMode: a mixer waits in update mode.
	FirstUpdateMode
	// FirstStorage: a mixer is in storage mode.
	FirstStorage
	// FirstTwo: more than one mixer.
	FirstTwo
	// FirstDuo: one Duo in normal mode, without RC2FX.
	FirstDuo
)

// FirstOf is the state the first screen names for what the app sees.
// Storage mode comes first: a mixer in it shows no other USB device.
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
